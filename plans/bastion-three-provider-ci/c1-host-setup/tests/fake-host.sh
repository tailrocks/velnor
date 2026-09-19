#!/usr/bin/env bash
set -euo pipefail

TEST_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
C1_DIR="$(cd -- "$TEST_DIR/.." && pwd)"
# shellcheck disable=SC1091
. "$C1_DIR/provision-bastion.sh"

assert_status() {
  local label="$1" expected="$2" actual
  shift 2
  if "$@" >/dev/null 2>&1; then actual=0; else actual=$?; fi
  if [[ "$actual" != "$expected" ]]; then
    printf 'FAIL %s: got status %s, expected %s\n' "$label" "$actual" "$expected" >&2
    exit 1
  fi
  printf 'PASS %s\n' "$label"
}

assert_equal() {
  local label="$1" expected="$2" actual="$3"
  if [[ "$actual" != "$expected" ]]; then
    printf 'FAIL %s\nexpected: %q\nactual:   %q\n' "$label" "$expected" "$actual" >&2
    exit 1
  fi
  printf 'PASS %s\n' "$label"
}

# shellcheck disable=SC2329 # running_containers discovers this stub via command -v.
docker() { return 1; }
assert_status 'docker ps failure is unknown, not zero containers' 2 running_containers
unset -f docker

preflight_apply_unknown() (
  CHECK=0
  WORK_INVENTORY_UNKNOWN=0
  # shellcheck disable=SC2329 # preflight_work resolves this scoped stub by name.
  running_containers() { return 2; }
  preflight_work
)
assert_status 'apply preflight refuses an unknown Docker work inventory' 2 \
  preflight_apply_unknown

preflight_check_unknown() (
  # shellcheck disable=SC2034 # read by preflight_work.
  CHECK=1
  WORK_INVENTORY_UNKNOWN=0
  # shellcheck disable=SC2329 # preflight_work resolves this scoped stub by name.
  running_containers() { return 2; }
  preflight_work
  [[ "$WORK_INVENTORY_UNKNOWN" == 1 ]]
)
assert_status 'check mode records incomplete Docker work inventory' 0 \
  preflight_check_unknown

drain_gate_unknown() (
  # shellcheck disable=SC2329 # require_drained resolves this scoped stub by name.
  running_containers() { return 2; }
  require_drained 'Docker package change'
)
assert_status 'restart gate refuses unknown Docker work inventory' 2 \
  drain_gate_unknown

assert_status 'supernet CIDR overlap' 0 cidr_overlaps 172.30.0.0/16 172.16.0.0/12
assert_status 'contained CIDR overlap' 0 cidr_overlaps 172.30.0.0/16 172.30.1.0/24
assert_status 'exact CIDR overlap' 0 cidr_overlaps 172.30.0.0/16 172.30.0.0/16
assert_status 'disjoint CIDR' 1 cidr_overlaps 172.30.0.0/16 172.29.0.0/16
assert_status 'zero-prefix route overlaps' 0 cidr_overlaps 172.30.0.0/16 0.0.0.0/0
assert_status 'invalid CIDR fails closed' 2 cidr_overlaps 172.30.0.0/16 172.30.0.0/33
assert_status 'invalid octet fails closed' 2 cidr_overlaps 172.30.0.0/16 172.300.0.0/16

ip_routes=$(route_cidrs_from_ip <<'ROUTES'
default via 192.0.2.1 dev eth0
172.16.0.0/12 dev eth1 proto kernel scope link src 172.16.0.10
local 127.0.0.0/8 dev lo table local proto kernel scope host src 127.0.0.1
2001:db8::/32 dev eth2 proto kernel
ROUTES
)
assert_equal 'IPv4 route extraction ignores gateways and IPv6' \
  $'172.16.0.0/12\n127.0.0.0/8' "$ip_routes"
assert_status 'malformed IPv4 route fails closed' 2 route_cidrs_from_ip \
  <<< '172.30.0.0/garbage dev eth0'

proc_routes=$(route_cidrs_from_proc <<'ROUTES'
Iface Destination Gateway Flags RefCnt Use Metric Mask MTU Window IRTT
eth0 00000000 01001AAC 0003 0 0 100 00000000 0 0 0
eth1 00001EAC 00000000 0001 0 0 0 0000FFFF 0 0 0
ROUTES
)
assert_equal '/proc/net/route little-endian parsing' \
  $'0.0.0.0/0\n172.30.0.0/16' "$proc_routes"

gpg() {
  [[ "${1:-}" == --show-keys && "${2:-}" == --with-colons ]] || return 2
  printf '%s\n' "${FAKE_GPG_OUTPUT:-}"
}

FAKE_GPG_OUTPUT=$'pub:-:1:1:8D81803C0EBFCD88:1700000000:::-:::scSC::::::23:\nfpr:::::::::9DC858229FC7DD38854AE2D88D81803C0EBFCD88:\nuid:-::::1700000000::0000000000000000000000000000000000000000::Docker Release <docker@docker.com>::::::::::0:\nsub:-:1:1:7EA0A9C3F273FCD8:1700000000::::::e::::::23:\nfpr:::::::::D3306A018370199E527AE7997EA0A9C3F273FCD8:'
expected_key_set=$'pub:9DC858229FC7DD38854AE2D88D81803C0EBFCD88\nsub:D3306A018370199E527AE7997EA0A9C3F273FCD8'
caller_key_set="$(
  env \
    VELNOR_C1_DOCKER_KEY_FPR=AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA \
    VELNOR_C1_DOCKER_KEY_SUB=BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB \
    bash -s "$C1_DIR/pins.env" <<'PIN_TEST'
. "$1"
printf '%s\n%s\n' "$VELNOR_C1_DOCKER_KEY_FPR" "$VELNOR_C1_DOCKER_KEY_SUB"
PIN_TEST
)"
assert_equal 'caller environment cannot override authenticated key fingerprints' \
  $'9DC858229FC7DD38854AE2D88D81803C0EBFCD88\nD3306A018370199E527AE7997EA0A9C3F273FCD8' \
  "$caller_key_set"
valid_key_set="$(key_fingerprint_records /fake/docker.gpg)"
assert_equal 'exact complete primary and subkey set' "$expected_key_set" "$valid_key_set"

FAKE_GPG_OUTPUT+=$'\nsub:-:1:1:AAAAAAAAAAAAAAAA:1700000000::::::e::::::23:\nfpr:::::::::AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA:'
appended_key_set="$(key_fingerprint_records /fake/docker.gpg)"
if [[ "$appended_key_set" == "$expected_key_set" ]]; then
  printf 'FAIL appended unexpected subkey was accepted\n' >&2
  exit 1
fi
printf 'PASS appended unexpected subkey rejected by exact-set comparison\n'

FAKE_GPG_OUTPUT+=$'\npub:-:1:1:BBBBBBBBBBBBBBBB:1700000000:::-:::scSC::::::23:\nfpr:::::::::BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB:'
appended_primary_set="$(key_fingerprint_records /fake/docker.gpg)"
if [[ "$appended_primary_set" == "$expected_key_set" ]]; then
  printf 'FAIL appended unexpected primary key was accepted\n' >&2
  exit 1
fi
printf 'PASS appended unexpected primary key rejected by exact-set comparison\n'

FAKE_GPG_OUTPUT=$'pub:-:1:1:8D81803C0EBFCD88::::::scSC::::::23:'
assert_status 'fingerprint without its record rejected' 4 key_fingerprint_records /fake/docker.gpg

FAKE_TMP="$(mktemp -d)"
trap 'rm -rf -- "$FAKE_TMP"' EXIT
literal_repo_line="deb https://example.invalid/debian \$(touch \"\$FAKE_TMP/injected\")"
write_text_file "$literal_repo_line" "$FAKE_TMP/source.list"
assert_equal 'APT source input is written literally' \
  "$literal_repo_line" "$(<"$FAKE_TMP/source.list")"
[[ ! -e "$FAKE_TMP/injected" ]] \
  || { printf 'FAIL APT source input executed as shell code\n' >&2; exit 1; }
printf 'PASS APT source input cannot execute shell code\n'

FAKE_APT_DIR="$FAKE_TMP/apt"
mkdir -p "$FAKE_APT_DIR"
FAKE_REPO_FILE="$FAKE_APT_DIR/docker.list"
FAKE_LEGACY_FILE="$FAKE_APT_DIR/docker-ce.list"
FAKE_DOCKER_REPO='deb [arch=amd64 signed-by=/etc/apt/keyrings/docker.asc] https://download.docker.com/linux/debian trixie stable'
# shellcheck disable=SC2329 # docker_repo_state resolves this fake stat by name.
stat() {
  [[ "$1" == -c && "$2" == %h && "$3" == -- ]] || return 2
  python3 - "$4" <<'STAT_TEST'
import os
import sys
print(os.lstat(sys.argv[1]).st_nlink)
STAT_TEST
}
repo_state="$(docker_repo_state "$FAKE_REPO_FILE" "$FAKE_LEGACY_FILE" "$FAKE_DOCKER_REPO")"
assert_equal 'missing Docker APT source is recognized' missing "$repo_state"
write_text_file "$FAKE_DOCKER_REPO" "$FAKE_REPO_FILE"
repo_state="$(docker_repo_state "$FAKE_REPO_FILE" "$FAKE_LEGACY_FILE" "$FAKE_DOCKER_REPO")"
assert_equal 'exact managed Docker APT source is preserved' current "$repo_state"
custom_repo='deb https://example.invalid/custom trixie stable'
write_text_file "$custom_repo" "$FAKE_REPO_FILE"
assert_status 'custom Docker source is rejected' 2 \
  docker_repo_state "$FAKE_REPO_FILE" "$FAKE_LEGACY_FILE" "$FAKE_DOCKER_REPO"
assert_equal 'custom Docker source remains untouched' \
  "$custom_repo" "$(<"$FAKE_REPO_FILE")"
write_text_file "$FAKE_DOCKER_REPO" "$FAKE_REPO_FILE"
ln "$FAKE_REPO_FILE" "$FAKE_TMP/docker-hardlink.list"
assert_status 'hardlinked Docker source is rejected' 2 \
  docker_repo_state "$FAKE_REPO_FILE" "$FAKE_LEGACY_FILE" "$FAKE_DOCKER_REPO"
rm -f -- "$FAKE_TMP/docker-hardlink.list"
write_text_file 'deb https://example.invalid/legacy trixie stable' "$FAKE_LEGACY_FILE"
assert_status 'legacy Docker source is rejected, not deleted' 2 \
  docker_repo_state "$FAKE_REPO_FILE" "$FAKE_LEGACY_FILE" "$FAKE_DOCKER_REPO"
assert_equal 'legacy Docker source remains untouched' \
  'deb https://example.invalid/legacy trixie stable' "$(<"$FAKE_LEGACY_FILE")"
rm -f -- "$FAKE_LEGACY_FILE" "$FAKE_REPO_FILE"
printf 'deb https://example.invalid/target trixie stable\n' > "$FAKE_TMP/target.list"
ln -s "$FAKE_TMP/target.list" "$FAKE_REPO_FILE"
assert_status 'symlinked Docker source is rejected' 2 \
  docker_repo_state "$FAKE_REPO_FILE" "$FAKE_LEGACY_FILE" "$FAKE_DOCKER_REPO"
assert_equal 'symlink target remains untouched' \
  'deb https://example.invalid/target trixie stable' "$(<"$FAKE_TMP/target.list")"
unset -f stat

FAKE_BIN="$FAKE_TMP/bin"
mkdir -p "$FAKE_BIN"
export FAKE_HOLDS_FILE="$FAKE_TMP/holds"
export FAKE_TRACE="$FAKE_TMP/trace"
printf 'docker-ce\ncontainerd.io\n' > "$FAKE_HOLDS_FILE"
: > "$FAKE_TRACE"

cat > "$FAKE_BIN/apt-mark" <<'FAKE_APT_MARK'
#!/usr/bin/env bash
set -euo pipefail
action="$1"
pkg="${2:-}"
case "$action" in
  showhold)
    cat "$FAKE_HOLDS_FILE"
    ;;
  unhold)
    printf 'unhold %s\n' "$pkg" >> "$FAKE_TRACE"
    grep -Fvx "$pkg" "$FAKE_HOLDS_FILE" > "$FAKE_HOLDS_FILE.tmp" || true
    mv "$FAKE_HOLDS_FILE.tmp" "$FAKE_HOLDS_FILE"
    ;;
  hold)
    printf 'hold %s\n' "$pkg" >> "$FAKE_TRACE"
    grep -Fqx "$pkg" "$FAKE_HOLDS_FILE" || printf '%s\n' "$pkg" >> "$FAKE_HOLDS_FILE"
    ;;
  *) exit 2 ;;
esac
FAKE_APT_MARK

cat > "$FAKE_BIN/apt-get" <<'FAKE_APT_GET'
#!/usr/bin/env bash
set -euo pipefail
printf 'apt-get %s\n' "$*" >> "$FAKE_TRACE"
[[ "$1" == install ]]
FAKE_APT_GET

chmod +x "$FAKE_BIN/apt-mark" "$FAKE_BIN/apt-get"
PATH="$FAKE_BIN:$PATH"
export PATH
C1_TEST_LOCK_WRAPPER_USED=0
pkg_installed() {
  [[ "$1" == velnor-runner ]]
}
require_drained() { return 0; }
apt_update_once() { :; }
mut() {
  if [[ "$1" == /usr/bin/flock ]]; then
    [[ "$2" == --exclusive && "$3" == --nonblock && "$4" == --no-fork ]] \
      || { printf 'unexpected package-lock flags\n' >&2; return 1; }
    [[ "$5" == /run/velnor/package-transaction.lock ]] \
      || { printf 'unexpected package-lock path\n' >&2; return 1; }
    C1_TEST_LOCK_WRAPPER_USED=1
    shift 5 # Fake the lock command; execute its child under fake apt tools.
  fi
  "$@"
}
step_docker_packages >/dev/null
[[ "$C1_TEST_LOCK_WRAPPER_USED" == 1 ]] \
  || { printf 'FAIL Docker package update lacked Velnor lock wrapper\n' >&2; exit 1; }
expected_holds=$'containerd.io\ndocker-buildx-plugin\ndocker-ce\ndocker-ce-cli\ndocker-compose-plugin'
actual_holds="$(sort "$FAKE_HOLDS_FILE")"
assert_equal 'held Docker packages restored after pinned install' "$expected_holds" "$actual_holds"
grep -Fqx 'unhold docker-ce' "$FAKE_TRACE"
grep -Fqx 'unhold containerd.io' "$FAKE_TRACE"
grep -Fq 'apt-get install -y docker-ce=' "$FAKE_TRACE"
printf 'PASS package hold transaction unholds, installs, and re-holds under one lock\n'
