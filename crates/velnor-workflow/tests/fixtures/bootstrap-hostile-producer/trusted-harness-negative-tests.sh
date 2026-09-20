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

# The producer archive contract is an exact base-owned census. These archives
# are disposable test data; no candidate binary, Docker daemon, or network is
# involved.
python3 - "$tmp" <<'PY'
import json
import sys
import warnings
import zipfile
from pathlib import Path

root = Path(sys.argv[1])
warnings.simplefilter("ignore", UserWarning)

def write_archive(name, members):
    with zipfile.ZipFile(root / name, "w", compression=zipfile.ZIP_DEFLATED) as archive:
        for member, data in members:
            archive.writestr(member, data)

files = [
    ("velnor-workflow", b"ELF-placeholder"),
    ("candidate-manifest.json", b"{}"),
]
write_archive("exact.zip", files)
write_archive("extra.zip", files + [("unexpected", b"no")])
write_archive("missing.zip", files[:1])
with zipfile.ZipFile(root / "crc.zip", "w", compression=zipfile.ZIP_STORED) as archive:
    archive.writestr("velnor-workflow", b"first")
    archive.writestr("candidate-manifest.json", b"{}")
with zipfile.ZipFile(root / "crc.zip") as archive:
    info = archive.getinfo("velnor-workflow")
raw = bytearray((root / "crc.zip").read_bytes())
data_start = info.header_offset + 30 + len(info.filename.encode()) + len(info.extra)
raw[data_start] ^= 1
(root / "crc.zip").write_bytes(raw)
with zipfile.ZipFile(root / "duplicate.zip", "w") as archive:
    archive.writestr("velnor-workflow", b"first")
    archive.writestr("velnor-workflow", b"second")

json.dump(
    {
        "schema": "velnor.bootstrap-producer-manifest.v1",
        "profile": "debug",
        "features": [],
        "platform": "linux-amd64",
        "repository": "tailrocks/velnor",
        "run_id": 7,
        "revision": "0" * 40,
        "closure": "0" * 64,
        "binary_sha256": "f" * 64,
    },
    (root / "forged-manifest.json").open("w", encoding="utf-8"),
)
PY
python3 -B "$checker" --exact-member velnor-workflow \
  --exact-member candidate-manifest.json "$tmp/exact.zip" >"$tmp/exact.json"
jq -e '.schema == "velnor.bootstrap-archive-exact.v1" and .status == "valid" and .files == 2' \
  "$tmp/exact.json" >/dev/null
for archive in extra missing duplicate crc; do
  set +e
  python3 -B "$checker" --exact-member velnor-workflow \
    --exact-member candidate-manifest.json "$tmp/$archive.zip" \
    >"$tmp/$archive.out" 2>"$tmp/$archive.err"
  archive_rc=$?
  set -e
  test "$archive_rc" -ne 0
done
set +e
python3 -B "$checker" --validate-json \
  "$script_dir/producer-manifest.schema.json" "$tmp/missing.json" \
  >"$tmp/manifest-missing.out" 2>"$tmp/manifest-missing.err"
manifest_rc=$?
set -e
test "$manifest_rc" -ne 0
# A schema-valid but forged digest must fail the base-owned identity predicate.
python3 -B "$checker" --validate-json \
  "$script_dir/producer-manifest.schema.json" "$tmp/forged-manifest.json" \
  >"$tmp/manifest-forged-schema.json"
set +e
jq -e --arg expected "0000000000000000000000000000000000000000000000000000000000000000" \
  '.binary_sha256 == $expected' "$tmp/forged-manifest.json" >/dev/null
forged_rc=$?
set -e
test "$forged_rc" -ne 0

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
