#!/usr/bin/env bash
set -euo pipefail
# shellcheck disable=SC2034 # many fake globals are read by the sourced provisioner.

TEST_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
C1_DIR="$(cd -- "$TEST_DIR/.." && pwd)"
# shellcheck disable=SC1091
. "$C1_DIR/provision-bastion.sh"

assert_status() {
  local label="$1" expected="$2" actual output_file status_pid
  shift 2
  output_file="$FAKE_TMP/assert-status.$$.${RANDOM}"
  "$@" <&0 >"$output_file" 2>&1 &
  status_pid=$!
  if wait "$status_pid"; then actual=0; else actual=$?; fi
  if [[ "$actual" != "$expected" ]]; then
    printf 'FAIL %s: got status %s, expected %s\n' "$label" "$actual" "$expected" >&2
    cat "$output_file" >&2
    rm -f -- "$output_file"
    if [[ "${FAKE_DEBUG_STATUS:-0}" == 1 ]]; then ( set -x; "$@" ); fi
    exit 1
  fi
  rm -f -- "$output_file"
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

copy_packaged_daemon_fragment() {
  local destination="$1" source_name="${2:-velnor-daemon.service}"
  cp -- "$C1_DIR/../../../crates/velnor-runner/debian/$source_name" "$destination"
}

FAKE_TMP="$(mktemp -d)"
FAKE_TMP="$(cd -P -- "$FAKE_TMP" && pwd -P)"
trap '[[ "${BASH_SUBSHELL:-0}" -ne 0 ]] || rm -rf -- "$FAKE_TMP"' EXIT

FAKE_STOCK_DAEMON_EXECSTART='/usr/bin/flock --shared --no-fork /run/velnor/package-transaction.lock /usr/bin/velnor-runner daemon --url ${VELNOR_URL} --name ${VELNOR_NAME} --labels ${VELNOR_LABELS} --slots ${VELNOR_SLOTS} --work-dir ${VELNOR_WORK_DIR} --replace'

docker_ps_failure_is_unknown_test() (
  # shellcheck disable=SC2329 # running_containers resolves this scoped fake CLI.
  docker_local() { return 1; }
  running_containers
)
assert_status 'docker ps failure is unknown, not zero containers' 2 \
  docker_ps_failure_is_unknown_test

missing_docker_info_check() (
  CHECK=1
  # shellcheck disable=SC2123 # hide every real CLI to exercise missing-Docker behavior.
  PATH=/nonexistent
  require_docker_info_snapshot
)
assert_status 'check mode treats a missing Docker CLI as fatal unknown' 2 \
  missing_docker_info_check

failed_docker_info_check() (
  CHECK=1
  # shellcheck disable=SC2329 # docker_info_snapshot resolves this scoped fake CLI.
  docker_local() { return 1; }
  require_docker_info_snapshot
)
assert_status 'check mode treats failed docker info as fatal unknown' 2 \
  failed_docker_info_check

valid_docker_info_snapshot() (
  # shellcheck disable=SC2329 # docker_info_snapshot resolves this scoped fake CLI.
  docker_local() {
    [[ "$1" == info && "$2" == --format ]] || return 2
    printf '/var/lib/docker|overlay2|json-file|systemd|2\n'
  }
  docker_info_snapshot
)
assert_equal 'complete Docker info snapshot is parsed' \
  '/var/lib/docker|overlay2|json-file|systemd|2' "$(valid_docker_info_snapshot)"

preflight_apply_unknown() (
  CHECK=0
  WORK_INVENTORY_UNKNOWN=0
  # shellcheck disable=SC2329 # preflight_work resolves this scoped stub by name.
  running_containers() { return 2; }
  # shellcheck disable=SC2329 # preflight_work resolves this scoped stub by name.
  docker_runtime_inactive() { return 1; }
  preflight_work
)
assert_status 'apply preflight refuses an unknown Docker work inventory' 2 \
  preflight_apply_unknown

preflight_apply_stopped_docker() (
  CHECK=0
  WORK_INVENTORY_UNKNOWN=0
  # shellcheck disable=SC2329 # preflight_work resolves these scoped stubs.
  running_containers() { return 2; }
  docker_runtime_inactive() { return 0; }
  preflight_work
  [[ "$WORK_INVENTORY_UNKNOWN" == 1 ]]
)
assert_status 'apply can continue from a confirmed stopped Docker runtime' 0 \
  preflight_apply_stopped_docker

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
  container_count_for_maintenance() { return 2; }
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

FAKE_ARCHIVE_FILE="$FAKE_TMP/archives/fake-package.deb"
mkdir -p "${FAKE_ARCHIVE_FILE%/*}"
printf 'fake package archive bytes\n' > "$FAKE_ARCHIVE_FILE"
FAKE_APT_ARCHIVE_SHA="$(sha256sum -- "$FAKE_ARCHIVE_FILE" | awk '{ print $1 }')"
export FAKE_ARCHIVE_FILE FAKE_APT_ARCHIVE_SHA
APT_SAFE_TEMP_ROOT="$FAKE_TMP"
# shellcheck disable=SC2329 # ensure_safe_apt_config resolves this fake ownership operation.
chown() { :; }
# shellcheck disable=SC2329 # provisioner functions resolve this portable chmod wrapper.
chmod() {
  local mode="$1"
  shift
  [[ "${1:-}" != -- ]] || shift
  command chmod "$mode" "$@"
}

assert_equal 'installed Velnor dpkg state is recognized' installed \
  "$(runner_dpkg_state 'install ok installed')"
assert_equal 'removed Velnor package is recognized' absent \
  "$(runner_dpkg_state 'deinstall ok config-files')"
assert_equal 'unknown never-installed package state is recognized' absent \
  "$(runner_dpkg_state 'unknown ok not-installed')"
assert_status 'partly unpacked runner state is unknown' 2 \
  runner_dpkg_state 'install ok unpacked'
assert_status 'half-configured runner state is unknown' 2 \
  runner_dpkg_state 'install ok half-configured'

docker_service_absent_is_bootstrap_safe_test() (
  DOCKER_SOCKET_PATHS=("$FAKE_TMP/docker.sock" "$FAKE_TMP/var-run-docker.sock")
  systemd_available() { return 0; }
  systemctl() {
    [[ "$1" == show && "$2" == --property=LoadState && "$3" == --value \
      && "$4" == docker.service ]] \
      || return 2
    printf 'not-found\n'
  }
  pgrep() { return 1; }
  docker_runtime_inactive
)
assert_status 'uninstalled Docker service with no process or socket permits bootstrap' 0 \
  docker_service_absent_is_bootstrap_safe_test

docker_service_without_systemd_is_unknown_test() (
  systemd_available() { return 1; }
  docker_service_inactive
)
assert_status 'missing systemd cannot prove Docker is stopped' 1 \
  docker_service_without_systemd_is_unknown_test

docker_service_active_is_unknown_test() (
  DOCKER_SOCKET_PATHS=("$FAKE_TMP/docker.sock")
  systemd_available() { return 0; }
  systemctl() {
    case "$2" in
      --property=LoadState) [[ "$3" == --value && "$4" == docker.service ]] || return 2; printf 'loaded\n' ;;
      --property=ActiveState) [[ "$3" == --value && "$4" == docker.service ]] || return 2; printf 'active\n' ;;
      *) return 2 ;;
    esac
  }
  pgrep() { return 1; }
  docker_runtime_inactive
)
assert_status 'active Docker service cannot be treated as stopped' 1 \
  docker_service_active_is_unknown_test

docker_unknown_systemd_state_is_fatal_test() (
  DOCKER_SOCKET_PATHS=("$FAKE_TMP/docker.sock")
  systemd_available() { return 0; }
  systemctl() {
    case "$2" in
      --property=LoadState) [[ "$3" == --value && "$4" == docker.service ]] || return 2; printf 'loaded\n' ;;
      --property=ActiveState) [[ "$3" == --value && "$4" == docker.service ]] || return 2; printf 'unknown\n' ;;
      *) return 2 ;;
    esac
  }
  pgrep() { return 1; }
  docker_runtime_inactive
)
assert_status 'unknown Docker service state fails closed' 1 \
  docker_unknown_systemd_state_is_fatal_test

docker_socket_blocks_bootstrap_test() (
  DOCKER_SOCKET_PATHS=("$FAKE_TMP/docker.sock")
  systemd_available() { return 0; }
  systemctl() {
    [[ "$1" == show && "$2" == --property=LoadState && "$3" == --value \
      && "$4" == docker.service ]] || return 2
    printf 'not-found\n'
  }
  pgrep() { return 1; }
  python3 - "$FAKE_TMP/docker.sock" <<'SOCKET_TEST'
import socket
import sys
sock = socket.socket(socket.AF_UNIX)
sock.bind(sys.argv[1])
sock.close()
SOCKET_TEST
  docker_runtime_inactive
)
assert_status 'Docker socket blocks stopped-runtime classification' 1 \
  docker_socket_blocks_bootstrap_test

preflight_missing_docker_check_is_unknown_test() (
  CHECK=1
  docker_info_snapshot() { return 2; }
  docker_runtime_inactive() { return 0; }
  preflight_docker_runtime
)
assert_status '--check rejects missing Docker info even when runtime looks stopped' 2 \
  preflight_missing_docker_check_is_unknown_test

preflight_missing_docker_apply_allows_stopped_test() (
  CHECK=0
  docker_info_snapshot() { return 2; }
  docker_runtime_inactive() { return 0; }
  preflight_docker_runtime
  [[ "$DOCKER_RUNTIME_UNKNOWN" == 1 ]]
)
assert_status 'apply accepts missing pre-install Docker only after stopped proof' 0 \
  preflight_missing_docker_apply_allows_stopped_test

postinstall_docker_health_unknown_test() (
  CHECK=0
  docker_info_snapshot() { return 2; }
  require_docker_info_snapshot
)
assert_status 'post-install Docker health still requires docker info' 2 \
  postinstall_docker_health_unknown_test

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
FAKE_STAT_OVERRIDE_PATH=
FAKE_STAT_OWNER_OVERRIDE=
FAKE_STAT_MODE_OVERRIDE=
# shellcheck disable=SC2329 # managed-file checks resolve this fake stat by name.
stat() {
  [[ "$1" == -c && "$3" == -- ]] || return 2
  local format="$2" path="$4" owner=0:0 mode='' links=''
  if [[ "$path" == "$FAKE_STAT_OVERRIDE_PATH" ]]; then
    owner="${FAKE_STAT_OWNER_OVERRIDE:-0:0}"
    mode="${FAKE_STAT_MODE_OVERRIDE:-}"
  fi
  if [[ -z "$mode" ]]; then
    if [[ -d "$path" ]]; then mode=755; else mode=644; fi
  fi
  links="$(python3 - "$path" <<'STAT_TEST'
import os
import sys
print(os.lstat(sys.argv[1]).st_nlink)
STAT_TEST
)" || return 2
  case "$format" in
    '%u:%g %a %h') printf '%s %s %s\n' "$owner" "$mode" "$links" ;;
    '%u:%g %a') printf '%s %s\n' "$owner" "$mode" ;;
    '%u:%g') printf '%s\n' "$owner" ;;
    '%a') printf '%s\n' "$mode" ;;
    '%h') printf '%s\n' "$links" ;;
    *) return 2 ;;
  esac
}
repo_state="$(docker_repo_state "$FAKE_REPO_FILE" "$FAKE_LEGACY_FILE" "$FAKE_DOCKER_REPO")"
assert_equal 'missing Docker APT source is recognized' missing "$repo_state"
write_text_file "$FAKE_DOCKER_REPO" "$FAKE_REPO_FILE"
repo_state="$(docker_repo_state "$FAKE_REPO_FILE" "$FAKE_LEGACY_FILE" "$FAKE_DOCKER_REPO")"
assert_equal 'exact managed Docker APT source is preserved' current "$repo_state"
FAKE_STAT_OVERRIDE_PATH="$FAKE_REPO_FILE"
FAKE_STAT_OWNER_OVERRIDE=1000:1000
assert_status 'content-equal Docker source with non-root owner is rejected' 2 \
  docker_repo_state "$FAKE_REPO_FILE" "$FAKE_LEGACY_FILE" "$FAKE_DOCKER_REPO"
FAKE_STAT_OWNER_OVERRIDE=
FAKE_STAT_MODE_OVERRIDE=666
assert_status 'content-equal Docker source with writable mode is rejected' 2 \
  docker_repo_state "$FAKE_REPO_FILE" "$FAKE_LEGACY_FILE" "$FAKE_DOCKER_REPO"
FAKE_STAT_OVERRIDE_PATH=
FAKE_STAT_MODE_OVERRIDE=
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

docker_key_metadata_tests() (
  local key_file="$FAKE_TMP/docker.asc" verified_key="$FAKE_TMP/verified-docker.asc"
  # The temporary root on macOS may be below the /var symlink; exercise key
  # metadata and traversal bits without treating that host path as the target.
  # shellcheck disable=SC2329 # assert_apt_key_readable_by_sandbox resolves this fake directory check.
  assert_safe_managed_directory() { [[ -d "$1" ]]; }
  write_text_file 'authenticated key bytes' "$key_file"
  write_text_file 'authenticated key bytes' "$verified_key"
  assert_equal 'content-equal Docker key with safe metadata is current' current \
    "$(docker_key_state "$key_file" "$verified_key")"
  FAKE_STAT_OVERRIDE_PATH="$key_file"
  FAKE_STAT_OWNER_OVERRIDE=1000:1000
  assert_status 'content-equal Docker key with non-root owner is rejected' 2 \
    docker_key_state "$key_file" "$verified_key"
  FAKE_STAT_OWNER_OVERRIDE=
  FAKE_STAT_MODE_OVERRIDE=600
  assert_status 'Docker key unreadable by _apt is rejected' 2 \
    docker_key_state "$key_file" "$verified_key"
  FAKE_STAT_MODE_OVERRIDE=666
  assert_status 'content-equal Docker key with writable mode is rejected' 2 \
    docker_key_state "$key_file" "$verified_key"
  FAKE_STAT_OVERRIDE_PATH="${key_file%/*}"
  FAKE_STAT_MODE_OVERRIDE=640
  assert_status 'Docker APT key parent not traversable by _apt is rejected' 2 \
    assert_apt_key_readable_by_sandbox "$key_file"
)
assert_status 'Docker APT key ownership, mode, and traversal checks' 0 docker_key_metadata_tests
FAKE_STAT_OVERRIDE_PATH=
FAKE_STAT_OWNER_OVERRIDE=
FAKE_STAT_MODE_OVERRIDE=

FAKE_DAEMON_FILE="$FAKE_TMP/daemon.json"
FAKE_DAEMON_WANT='{"log-opts":{"max-size":"10m"}}'
write_text_file "$FAKE_DAEMON_WANT" "$FAKE_DAEMON_FILE"
assert_equal 'content-equal daemon config with safe metadata is current' current \
  "$(daemon_config_state "$FAKE_DAEMON_FILE" "$FAKE_DAEMON_WANT")"
FAKE_STAT_OVERRIDE_PATH="$FAKE_DAEMON_FILE"
FAKE_STAT_OWNER_OVERRIDE=1000:1000
assert_status 'content-equal daemon config with non-root owner is rejected' 2 \
  daemon_config_state "$FAKE_DAEMON_FILE" "$FAKE_DAEMON_WANT"
FAKE_STAT_OWNER_OVERRIDE=
FAKE_STAT_MODE_OVERRIDE=666
assert_status 'content-equal daemon config with writable mode is rejected' 2 \
  daemon_config_state "$FAKE_DAEMON_FILE" "$FAKE_DAEMON_WANT"

FAKE_STAT_OVERRIDE_PATH="$FAKE_APT_DIR"
FAKE_STAT_MODE_OVERRIDE=750
assert_status 'Velnor runtime directory accepts packaged mode 0750' 0 \
  assert_velnor_runtime_directory "$FAKE_APT_DIR"
FAKE_STAT_OWNER_OVERRIDE=1000:1000
assert_status 'Velnor runtime directory rejects non-root ownership' 2 \
  assert_velnor_runtime_directory "$FAKE_APT_DIR"
FAKE_STAT_OWNER_OVERRIDE=
FAKE_STAT_MODE_OVERRIDE=755
assert_status 'Velnor runtime directory rejects mode drift from tmpfiles' 2 \
  assert_velnor_runtime_directory "$FAKE_APT_DIR"
FAKE_STAT_MODE_OVERRIDE=777
assert_status 'writable APT source directory is rejected' 2 \
  assert_safe_managed_directory "$FAKE_APT_DIR" 'APT source directory'
FAKE_STAT_OVERRIDE_PATH=
FAKE_STAT_MODE_OVERRIDE=

FAKE_BIN="$FAKE_TMP/bin"
mkdir -p "$FAKE_BIN"
C1_TEST_REAL_TAR="$(command -v tar)"
C1_TEST_REAL_FIND="$(command -v find)"
export C1_TEST_REAL_TAR C1_TEST_REAL_FIND
cat > "$FAKE_BIN/docker" <<'FAKE_DOCKER'
#!/usr/bin/env bash
exit 1
FAKE_DOCKER
cat > "$FAKE_BIN/stat" <<'FAKE_STAT'
#!/usr/bin/env bash
set -euo pipefail
[[ "$1" == -c && "$2" == '%u:%g %a %h' && "$3" == -- && -f "$4" ]] || exit 2
printf '0:0 600 1\n'
FAKE_STAT
export FAKE_HOLDS_FILE="$FAKE_TMP/holds"
export FAKE_TRACE="$FAKE_TMP/trace"
export FAKE_FAIL_SHOWHOLD="$FAKE_TMP/fail-showhold"
export FAKE_REQUIRE_PACKAGE_LOCK=1
export FAKE_PACKAGE_LOCKED=0
export FAKE_LOCK_STATE_FILE="$FAKE_TMP/fake-lock-state"
export FAKE_FLOCK_BUSY_FILE="$FAKE_TMP/fake-flock-busy-count"
export FAKE_DPKG_STATE_FILE="$FAKE_TMP/dpkg-state"
export FAKE_APT_CACHE_MODE=valid
export FAKE_DOCKER_REPO_URL="$VELNOR_C1_DOCKER_REPO_URL"
export FAKE_DOCKER_DIST="$VELNOR_C1_DOCKER_DIST"
export FAKE_DOCKER_COMPONENT="$VELNOR_C1_DOCKER_COMPONENT"
# shellcheck disable=SC2034 # fake apt-cache executable reads exported pin values.
export FAKE_DOCKER_CE_PIN="$VELNOR_C1_DOCKER_CE_VERSION"
# shellcheck disable=SC2034 # fake apt-cache executable reads exported pin values.
export FAKE_DOCKER_CLI_PIN="$VELNOR_C1_DOCKER_CE_CLI_VERSION"
# shellcheck disable=SC2034 # fake apt-cache executable reads exported pin values.
export FAKE_CONTAINERD_PIN="$VELNOR_C1_CONTAINERD_VERSION"
# shellcheck disable=SC2034 # fake apt-cache executable reads exported pin values.
export FAKE_BUILDX_PIN="$VELNOR_C1_BUILDX_VERSION"
# shellcheck disable=SC2034 # fake apt-cache executable reads exported pin values.
export FAKE_COMPOSE_PIN="$VELNOR_C1_COMPOSE_VERSION"
export FAKE_DPKG_STATUS='install ok not-installed'
: > "$FAKE_LOCK_STATE_FILE"
: > "$FAKE_DPKG_STATE_FILE"
printf 'docker-ce\ncontainerd.io\n' > "$FAKE_HOLDS_FILE"
: > "$FAKE_TRACE"

cat > "$FAKE_BIN/apt-mark" <<'FAKE_APT_MARK'
#!/usr/bin/env bash
set -euo pipefail
assert_safe_apt_config() {
  [[ -n "${APT_CONFIG:-}" && -f "$APT_CONFIG" \
    && "$APT_CONFIG" != "${FAKE_POISON_APT_CONFIG:-/__not_a_poison_config__}" ]] || exit 90
  grep -Fqx 'Dir::Etc::main "/dev/null";' "$APT_CONFIG" || exit 91
  grep -Fqx "Dir::Etc::sourcelist \"$FAKE_EXPECTED_APT_SOURCELIST\";" "$APT_CONFIG" || exit 92
  grep -Fqx "Dir::Etc::sourceparts \"$FAKE_EXPECTED_APT_SOURCEPARTS\";" "$APT_CONFIG" || exit 93
  grep -Fqx 'Acquire::AllowInsecureRepositories "false";' "$APT_CONFIG" || exit 94
  grep -Fqx 'Acquire::AllowDowngradeToInsecureRepositories "false";' "$APT_CONFIG" || exit 95
  grep -Fqx 'Acquire::AllowWeakRepositories "false";' "$APT_CONFIG" || exit 96
  grep -Fqx 'APT::Get::AllowUnauthenticated "false";' "$APT_CONFIG" || exit 97
  grep -Fqx 'Debug::NoLocking "false";' "$APT_CONFIG" || exit 98
  local parts
  parts="$(sed -n 's/^Dir::Etc::Parts "\(.*\)";$/\1/p' "$APT_CONFIG")"
  [[ "$parts" == "${APT_CONFIG%/*}/parts" && -d "$parts" ]] || exit 99
  [[ -z "$(find "$parts" -mindepth 1 -maxdepth 1 -print -quit)" ]] || exit 100
}
assert_safe_apt_config
action="$1"
shift
if [[ "$FAKE_REQUIRE_PACKAGE_LOCK" == 1 \
  && "${FAKE_PACKAGE_LOCKED:-0}" != 1 \
  && "$(<"$FAKE_LOCK_STATE_FILE")" != shared \
  && "$(<"$FAKE_LOCK_STATE_FILE")" != exclusive ]]; then
  printf 'apt-mark called outside package lock\n' >&2
  exit 97
fi
case "$action" in
  showhold)
    printf 'apt-mark showhold\n' >> "$FAKE_TRACE"
    [[ ! -e "$FAKE_FAIL_SHOWHOLD" ]] || exit 42
    cat "$FAKE_HOLDS_FILE"
    ;;
  unhold)
    for pkg in "$@"; do
      printf 'unhold %s\n' "$pkg" >> "$FAKE_TRACE"
      grep -Fvx "$pkg" "$FAKE_HOLDS_FILE" > "$FAKE_HOLDS_FILE.tmp" || true
      mv "$FAKE_HOLDS_FILE.tmp" "$FAKE_HOLDS_FILE"
    done
    ;;
  hold)
    for pkg in "$@"; do
      printf 'hold %s\n' "$pkg" >> "$FAKE_TRACE"
      grep -Fqx "$pkg" "$FAKE_HOLDS_FILE" || printf '%s\n' "$pkg" >> "$FAKE_HOLDS_FILE"
    done
    ;;
  *) exit 2 ;;
esac
FAKE_APT_MARK

cat > "$FAKE_BIN/apt-get" <<'FAKE_APT_GET'
#!/usr/bin/env bash
set -euo pipefail
assert_safe_apt_config() {
  [[ -n "${APT_CONFIG:-}" && -f "$APT_CONFIG" \
    && "$APT_CONFIG" != "${FAKE_POISON_APT_CONFIG:-/__not_a_poison_config__}" ]] || exit 90
  grep -Fqx 'Dir::Etc::main "/dev/null";' "$APT_CONFIG" || exit 91
  grep -Fqx "Dir::Etc::sourcelist \"$FAKE_EXPECTED_APT_SOURCELIST\";" "$APT_CONFIG" || exit 92
  grep -Fqx "Dir::Etc::sourceparts \"$FAKE_EXPECTED_APT_SOURCEPARTS\";" "$APT_CONFIG" || exit 93
  grep -Fqx 'Acquire::AllowInsecureRepositories "false";' "$APT_CONFIG" || exit 94
  grep -Fqx 'Acquire::AllowDowngradeToInsecureRepositories "false";' "$APT_CONFIG" || exit 95
  grep -Fqx 'Acquire::AllowWeakRepositories "false";' "$APT_CONFIG" || exit 96
  grep -Fqx 'APT::Get::AllowUnauthenticated "false";' "$APT_CONFIG" || exit 97
  grep -Fqx 'Debug::NoLocking "false";' "$APT_CONFIG" || exit 98
  local parts
  parts="$(sed -n 's/^Dir::Etc::Parts "\(.*\)";$/\1/p' "$APT_CONFIG")"
  [[ "$parts" == "${APT_CONFIG%/*}/parts" && -d "$parts" ]] || exit 99
  [[ -z "$(find "$parts" -mindepth 1 -maxdepth 1 -print -quit)" ]] || exit 100
}
assert_safe_apt_config
[[ "$FAKE_REQUIRE_PACKAGE_LOCK" == 1 \
  && ( "${FAKE_PACKAGE_LOCKED:-0}" == 1 \
    || "$(<"$FAKE_LOCK_STATE_FILE")" == shared \
    || "$(<"$FAKE_LOCK_STATE_FILE")" == exclusive ) ]] \
  || { printf 'apt-get called outside package lock\n' >&2; exit 97; }
printf 'apt-get %s\n' "$*" >> "$FAKE_TRACE"
apt_options=()
apt_args=()
while [[ "$#" -gt 0 ]]; do
  if [[ "$1" == -o ]]; then
    [[ "$#" -ge 2 ]] || exit 2
    apt_options+=("$2")
    shift 2
  else
    apt_args+=("$1")
    shift
  fi
done
set -- "${apt_args[@]}"
printf 'apt-get invocation %s\n' "$*" >> "$FAKE_TRACE"
hook_option=
hook_version=
for option in "${apt_options[@]}"; do
  case "$option" in
    DPkg::Pre-Install-Pkgs::=*) hook_option="${option#*=}" ;;
    DPkg::Tools::Options::*::Version=*) hook_version="$option" ;;
  esac
done
case "$1" in
  update)
    [[ -z "${FAKE_FAIL_APT_UPDATE:-}" || ! -e "$FAKE_FAIL_APT_UPDATE" ]] || exit 41
    ;;
  --simulate)
    shift
    while [[ "${1:-}" == --* || "${1:-}" == -y ]]; do shift; done
    [[ "${1:-}" == install ]] || exit 2
    shift
    [[ -n "${FAKE_APT_SIMULATE_OMIT:-}" ]] || {
      for spec in "$@"; do
        pkg="${spec%%=*}"
        version="${spec#*=}"
        if [[ "$spec" != *=* ]]; then version="${FAKE_DEBIAN_PACKAGE_VERSION:-1.0-1}"; fi
        old_version="$(awk -F'|' -v pkg="$pkg" '$1 == pkg && $2 ~ / installed$/ { print $3; exit }' "$FAKE_DPKG_STATE_FILE")"
        if [[ -n "$old_version" ]]; then
          printf 'Inst %s [%s] (%s Debian:13/stable [amd64])\n' "$pkg" "$old_version" "$version"
        else
          printf 'Inst %s (%s Debian:13/stable [amd64])\n' "$pkg" "$version"
        fi
        printf 'Conf %s (%s Debian:13/stable [amd64])\n' "$pkg" "$version"
      done
    }
    if [[ "${FAKE_APT_SIMULATE_DEP_UPGRADE:-0}" == 1 ]]; then
      printf 'Inst fake-existing-dependency [1.0-1] (2.0-1 Debian:13/stable [amd64])\n'
      printf 'Conf fake-existing-dependency (2.0-1 Debian:13/stable [amd64])\n'
    fi
    [[ "${FAKE_APT_SIMULATE_REMOVE:-0}" != 1 ]] || printf 'Remv fake-conflict [1.0-1]\n'
    ;;
  install)
    if [[ "${FAKE_APT_RUN_PLAN_HOOK:-0}" == 1 ]]; then
      [[ -n "$hook_option" && "$hook_version" == DPkg::Tools::Options::*::Version=2 ]] \
        || { printf 'APT plan hook/version was not attached to install\n' >&2; exit 94; }
      read -r hook plan_file <<< "$hook_option"
      [[ -x "$hook" && -f "$plan_file" && -n "${FAKE_APT_HOOK_INPUT:-}" ]] || exit 95
      if [[ "${FAKE_APT_GENERATE_HOOK_INPUT:-0}" == 1 ]]; then
        {
          printf 'VERSION 2\n'
          printf 'Dir::Etc::sourcelist=%s\n' "$FAKE_EXPECTED_APT_SOURCELIST"
          printf 'Dir::Etc::sourceparts=%s\n' "$FAKE_EXPECTED_APT_SOURCEPARTS"
          printf 'Acquire::AllowInsecureRepositories=false\n'
          printf 'Acquire::AllowDowngradeToInsecureRepositories=false\n'
          printf 'Acquire::AllowWeakRepositories=false\n'
          printf 'APT::Get::AllowUnauthenticated=false\n'
          printf 'Debug::NoLocking=false\n'
          printf 'APT::Architecture=amd64\n\n'
          while IFS=$'\t' read -r kind action pkg old direction version extra; do
            [[ "$kind" == ACTION ]] || continue
            case "$action" in
              UNPACK)
                printf '%s %s %s %s %s\n' \
                  "$pkg" "$old" "$direction" "$version" "$FAKE_ARCHIVE_FILE"
                ;;
              CONFIGURE)
                printf '%s %s %s %s **CONFIGURE**\n' \
                  "$pkg" "$old" "$direction" "$version"
                ;;
            esac
          done < "$plan_file"
        } > "$FAKE_APT_HOOK_INPUT"
      fi
      [[ -f "$FAKE_APT_HOOK_INPUT" ]] || exit 95
      "$hook" "$plan_file" < "$FAKE_APT_HOOK_INPUT" >> "$FAKE_TRACE" 2>&1
    fi
    if [[ "${FAKE_APT_PARTIAL_INSTALL:-0}" == 1 ]]; then
      installed=0
      for spec in "$@"; do
        [[ "$spec" == *=* ]] || continue
        pkg="${spec%%=*}"
        printf '%s|install ok installed\n' "$pkg" >> "$FAKE_DPKG_STATE_FILE"
        installed=$((installed + 1))
        (( installed < 2 )) || break
      done
    fi
    [[ -z "${FAKE_FAIL_APT_INSTALL:-}" || ! -e "$FAKE_FAIL_APT_INSTALL" ]] || exit 42
    ;;
  *) exit 2 ;;
esac
FAKE_APT_GET

cat > "$FAKE_BIN/dpkg-query" <<'FAKE_DPKG_QUERY'
#!/usr/bin/env bash
set -euo pipefail
pkg="${*: -1}"
status=""
if [[ -f "${FAKE_DPKG_STATE_FILE:-}" ]]; then
  status="$(awk -F'|' -v pkg="$pkg" '$1 == pkg { print $2; exit }' "$FAKE_DPKG_STATE_FILE")"
fi
if [[ "$*" == *'${Version}'* ]]; then
  version="$(awk -F'|' -v pkg="$pkg" '$1 == pkg { print $3; exit }' "${FAKE_DPKG_STATE_FILE:-/dev/null}")"
  printf '%s\n' "${version:-${FAKE_DPKG_VERSION:-}}"
else
  printf '%s\n' "${status:-${FAKE_DPKG_STATUS:-install ok not-installed}}"
fi
exit "${FAKE_DPKG_QUERY_STATUS:-0}"
FAKE_DPKG_QUERY

cat > "$FAKE_BIN/dpkg" <<'FAKE_DPKG'
#!/usr/bin/env bash
set -euo pipefail
if [[ "${1:-}" == --print-architecture ]]; then
  printf '%s\n' "${FAKE_NATIVE_ARCHITECTURE:-amd64}"
  exit 0
fi
[[ "${1:-}" == --compare-versions && "${4:-}" != "" ]] || exit 2
left="${2:-}"
right="${4:-}"
if [[ "$left" == "$right" ]]; then
  [[ "${3:-}" == eq ]]
  exit
fi
if [[ "${FAKE_DPKG_COMPARE_GT:-0}" == 1 ]]; then
  [[ "${3:-}" == gt ]]
  exit
fi
if [[ "$left" == 1.0-1 && "$right" != 1.0-1 ]]; then
  [[ "${3:-}" == lt ]]
  exit
fi
if [[ "$left" != 1.0-1 && "$right" == 1.0-1 ]]; then
  [[ "${3:-}" == gt ]]
  exit
fi
case "${3:-}" in
  gt) [[ "${FAKE_DPKG_COMPARE_GT:-0}" == 1 ]] ;;
  lt) [[ "${FAKE_DPKG_COMPARE_LT:-0}" == 1 ]] ;;
  eq) [[ "${FAKE_DPKG_COMPARE_EQ:-0}" == 1 ]] ;;
  *) exit 2 ;;
esac
FAKE_DPKG

cat > "$FAKE_BIN/apt-cache" <<'FAKE_APT_CACHE'
#!/usr/bin/env bash
set -euo pipefail
assert_safe_apt_config() {
  [[ -n "${APT_CONFIG:-}" && -f "$APT_CONFIG" \
    && "$APT_CONFIG" != "${FAKE_POISON_APT_CONFIG:-/__not_a_poison_config__}" ]] || exit 90
  grep -Fqx 'Dir::Etc::main "/dev/null";' "$APT_CONFIG" || exit 91
  grep -Fqx "Dir::Etc::sourcelist \"$FAKE_EXPECTED_APT_SOURCELIST\";" "$APT_CONFIG" || exit 92
  grep -Fqx "Dir::Etc::sourceparts \"$FAKE_EXPECTED_APT_SOURCEPARTS\";" "$APT_CONFIG" || exit 93
  grep -Fqx 'Acquire::AllowInsecureRepositories "false";' "$APT_CONFIG" || exit 94
  grep -Fqx 'Acquire::AllowDowngradeToInsecureRepositories "false";' "$APT_CONFIG" || exit 95
  grep -Fqx 'Acquire::AllowWeakRepositories "false";' "$APT_CONFIG" || exit 96
  grep -Fqx 'APT::Get::AllowUnauthenticated "false";' "$APT_CONFIG" || exit 97
  grep -Fqx 'Debug::NoLocking "false";' "$APT_CONFIG" || exit 98
  local parts
  parts="$(sed -n 's/^Dir::Etc::Parts "\(.*\)";$/\1/p' "$APT_CONFIG")"
  [[ "$parts" == "${APT_CONFIG%/*}/parts" && -d "$parts" ]] || exit 99
  [[ -z "$(find "$parts" -mindepth 1 -maxdepth 1 -print -quit)" ]] || exit 100
}
assert_safe_apt_config
[[ "$FAKE_REQUIRE_PACKAGE_LOCK" == 1 \
  && ( "${FAKE_PACKAGE_LOCKED:-0}" == 1 \
    || "$(<"$FAKE_LOCK_STATE_FILE")" == shared \
    || "$(<"$FAKE_LOCK_STATE_FILE")" == exclusive ) ]] \
  || { printf 'apt-cache called outside package lock\n' >&2; exit 97; }
[[ -n "${1:-}" && -n "${2:-}" ]] || exit 2
if [[ "$1" == show ]]; then
  spec="$2"
  pkg="${spec%%=*}"
  version="${spec#*=}"
  case "$pkg" in
    docker-ce) version="$FAKE_DOCKER_CE_PIN" ;;
    docker-ce-cli) version="$FAKE_DOCKER_CLI_PIN" ;;
    containerd.io) version="$FAKE_CONTAINERD_PIN" ;;
    docker-buildx-plugin) version="$FAKE_BUILDX_PIN" ;;
    docker-compose-plugin) version="$FAKE_COMPOSE_PIN" ;;
    *) version="${version:-${FAKE_DEBIAN_PACKAGE_VERSION:-1.0-1}}" ;;
  esac
  printf 'Package: %s\nVersion: %s\nArchitecture: amd64\nSHA256: %s\n\n' \
    "$pkg" "$version" "$FAKE_APT_ARCHIVE_SHA"
  exit 0
fi
[[ "$1" == policy ]] || exit 2
pkg="$2"
case "$pkg" in
  docker-ce) version="$FAKE_DOCKER_CE_PIN" ;;
  docker-ce-cli) version="$FAKE_DOCKER_CLI_PIN" ;;
  containerd.io) version="$FAKE_CONTAINERD_PIN" ;;
  docker-buildx-plugin) version="$FAKE_BUILDX_PIN" ;;
  docker-compose-plugin) version="$FAKE_COMPOSE_PIN" ;;
  *)
    version="${FAKE_DEBIAN_PACKAGE_VERSION:-1.0-1}"
    repo="${FAKE_DEBIAN_REPO_URL:-https://deb.debian.org/debian}"
    suite='trixie/main'
    case "${FAKE_APT_CACHE_MODE:-valid}" in
      wrong-candidate) candidate=0:0.0.0-1 ;;
      wrong-repo) repo=https://packages.attacker.invalid/debian ;;
      wrong-suite) suite=bookworm/main ;;
    esac
    candidate="${candidate:-$version}"
    printf '%s:\n Installed: (none)\n Candidate: %s\n Version table:\n' "$pkg" "$candidate"
    printf ' *** %s 500\n        500 %s %s amd64 Packages\n' "$version" "$repo" "$suite"
    if [[ "${FAKE_APT_CACHE_MODE:-valid}" == duplicate-origin ]]; then
      printf '        500 https://packages.attacker.invalid/debian %s amd64 Packages\n' "$suite"
    fi
    exit 0
    ;;
esac
candidate="$version"
repo="$FAKE_DOCKER_REPO_URL"
suite="$FAKE_DOCKER_DIST/$FAKE_DOCKER_COMPONENT"
case "${FAKE_APT_CACHE_MODE:-valid}" in
  wrong-candidate) candidate=0:0.0.0-1 ;;
  wrong-repo) repo=https://packages.attacker.invalid/docker ;;
  wrong-suite) suite=bookworm/stable ;;
esac
printf '%s:\n Installed: (none)\n Candidate: %s\n Version table:\n' "$pkg" "$candidate"
printf ' *** %s 500\n        500 %s %s amd64 Packages\n' "$version" "$repo" "$suite"
if [[ "${FAKE_APT_CACHE_MODE:-valid}" == duplicate-origin ]]; then
  printf '        500 https://packages.attacker.invalid/docker %s amd64 Packages\n' "$suite"
fi
FAKE_APT_CACHE

cat > "$FAKE_BIN/flock" <<'FAKE_FLOCK'
#!/usr/bin/env bash
set -euo pipefail
case "${1:-}" in
  --exclusive)
    shift
    [[ "${1:-}" != --nonblock ]] || shift
    if [[ -f "${FAKE_FLOCK_BUSY_FILE:-}" ]]; then
      remaining="$(<"$FAKE_FLOCK_BUSY_FILE")"
      if [[ "$remaining" =~ ^[0-9]+$ ]] && (( remaining > 0 )); then
        printf '%s\n' "$((remaining - 1))" > "$FAKE_FLOCK_BUSY_FILE"
        printf 'flock contended %s\n' "${1:-}" >> "$FAKE_TRACE"
        exit 1
      fi
    fi
    [[ ! -s "$FAKE_LOCK_STATE_FILE" ]] || exit 1
    printf 'exclusive\n' > "$FAKE_LOCK_STATE_FILE"
    printf 'flock exclusive %s\n' "${1:-}" >> "$FAKE_TRACE"
    FAKE_PACKAGE_LOCKED=1
    export FAKE_PACKAGE_LOCKED
    if [[ "${FAKE_REOPEN_ADMISSION_ON_LOCK:-0}" == 1 ]]; then
      printf 'velnor-guardian.service\n' >> "$FAKE_ACTIVE_FILE"
    fi
    ;;
  --shared)
    shift
    while [[ "${1:-}" == --no-fork || "${1:-}" == --nonblock ]]; do shift; done
    lock_path="$1"
    shift
    if [[ "$lock_path" =~ ^[0-9]+$ ]]; then lock_label=fd; else lock_label="$lock_path"; fi
    [[ "$(<"$FAKE_LOCK_STATE_FILE")" != exclusive ]] || {
      printf 'flock shared blocked %s\n' "$lock_label" >> "$FAKE_TRACE"
      exit 1
    }
    printf 'flock shared %s\n' "$lock_label" >> "$FAKE_TRACE"
    printf 'shared\n' > "$FAKE_LOCK_STATE_FILE"
    if [[ "$#" -gt 0 ]]; then
      if FAKE_PACKAGE_LOCKED=1 "$@"; then status=0; else status=$?; fi
      : > "$FAKE_LOCK_STATE_FILE"
      exit "$status"
    fi
    ;;
  --unlock)
    printf 'flock unlock %s\n' "${2:-}" >> "$FAKE_TRACE"
    : > "$FAKE_LOCK_STATE_FILE"
    FAKE_PACKAGE_LOCKED=0
    export FAKE_PACKAGE_LOCKED
    ;;
  *) exit 2 ;;
esac
FAKE_FLOCK

chmod +x "$FAKE_BIN/apt-mark" "$FAKE_BIN/apt-get" "$FAKE_BIN/apt-cache" \
  "$FAKE_BIN/dpkg-query" "$FAKE_BIN/dpkg" "$FAKE_BIN/flock" "$FAKE_BIN/docker" "$FAKE_BIN/stat"
PATH="$FAKE_BIN:$PATH"
export PATH

docker_local_endpoint_ignores_poisoned_context_test() (
  local docker_bin="$FAKE_TMP/docker-local-bin/docker" snapshot count
  mkdir -p "${docker_bin%/*}"
  cat > "$docker_bin" <<'LOCAL_DOCKER'
#!/usr/bin/env bash
set -euo pipefail
[[ -z "${DOCKER_HOST+x}${DOCKER_CONTEXT+x}${DOCKER_CONFIG+x}" ]] || exit 91
[[ "${1:-}" == --host && "${2:-}" == unix:///run/docker.sock ]] || exit 92
printf '%s\n' "$*" >> "$FAKE_DOCKER_TRACE"
shift 2
case "${1:-}" in
  ps)
    shift
    case "${1:-}" in
      -q) printf 'local-container-id\n' ;;
      --format) printf 'local-container-id image running\n' ;;
      *) exit 2 ;;
    esac
    ;;
  info)
    printf '/var/lib/docker|overlay2|json-file|systemd|2\n'
    ;;
  *) exit 2 ;;
esac
LOCAL_DOCKER
  chmod +x "$docker_bin"
  export FAKE_DOCKER_TRACE="$FAKE_TMP/docker-local.trace"
  : > "$FAKE_DOCKER_TRACE"
  export DOCKER_HOST=tcp://poison.invalid:2375
  export DOCKER_CONTEXT=untrusted-remote
  export DOCKER_CONFIG="$FAKE_TMP/remote-docker-config"
  PATH="${docker_bin%/*}:/usr/bin:/bin"
  export PATH
  # This focused endpoint test avoids the production /usr/bin/timeout path;
  # the fake docker itself is deterministic and returns immediately.
  # shellcheck disable=SC2329 # docker_local calls this command wrapper.
  maintenance_command() { "$@"; }
  systemd_available() { return 1; }
  active_velnor_units() { :; }
  systemctl() { return 0; }
  count="$(running_containers)"
  [[ "$count" == 1 ]]
  snapshot="$(docker_info_snapshot)"
  [[ "$snapshot" == '/var/lib/docker|overlay2|json-file|systemd|2' ]]
  preflight_work >/dev/null
  local expected
  expected=$'--host unix:///run/docker.sock ps -q\n--host unix:///run/docker.sock info --format {{.DockerRootDir}}|{{.Driver}}|{{.LoggingDriver}}|{{.CgroupDriver}}|{{.CgroupVersion}}\n--host unix:///run/docker.sock ps -q\n--host unix:///run/docker.sock ps --format {{.ID}} {{.Image}} {{.Status}}'
  [[ "$(<"$FAKE_DOCKER_TRACE")" == "$expected" ]]
)
assert_status 'poisoned Docker host/context cannot redirect inventory or health reads' 0 \
  docker_local_endpoint_ignores_poisoned_context_test

prepare_docker_origin_fixture() {
  # Package fake executables return immediately; this keeps the pure suite
  # portable where GNU /usr/bin/timeout is not installed.
  # shellcheck disable=SC2329 # apt_command and package_transaction use this wrapper.
  maintenance_command() { "$@"; }
  # Each fixture gets the original authenticated archive bytes; one mutation
  # regression must not poison later tests through their shared temp path.
  printf 'fake package archive bytes\n' > "$FAKE_ARCHIVE_FILE"
  FAKE_APT_ARCHIVE_SHA="$(sha256sum -- "$FAKE_ARCHIVE_FILE" | awk '{ print $1 }')"
  export FAKE_APT_ARCHIVE_SHA
  DOCKER_APT_SOURCE_DIR="$FAKE_APT_DIR/origin-sources"
  DOCKER_APT_SOURCE_FILE="$DOCKER_APT_SOURCE_DIR/docker.list"
  DOCKER_APT_LEGACY_SOURCE_FILE="$DOCKER_APT_SOURCE_DIR/docker-ce.list"
  DOCKER_APT_MAIN_SOURCE_FILE="$FAKE_APT_DIR/origin-main.list"
  DOCKER_APT_KEY_FILE="$FAKE_APT_DIR/origin-docker.asc"
  PACKAGE_LOCK_PATH="$FAKE_TMP/velnor/package-transaction.lock"
  mkdir -p "${PACKAGE_LOCK_PATH%/*}"
  : > "$PACKAGE_LOCK_PATH"
  : > "$FAKE_DPKG_STATE_FILE"
  : > "$FAKE_HOLDS_FILE"
  rm -rf -- "$DOCKER_APT_SOURCE_DIR"
  mkdir -p "$DOCKER_APT_SOURCE_DIR"
  write_text_file "$(docker_apt_repo_line)" "$DOCKER_APT_SOURCE_FILE"
  write_text_file 'authenticated Docker key' "$DOCKER_APT_KEY_FILE"
  cat > "$DOCKER_APT_SOURCE_DIR/debian.sources" <<'DEBIAN_SOURCES'
Types: deb
URIs: https://deb.debian.org/debian
Suites: trixie
Components: main
Signed-By: /usr/share/keyrings/debian-archive-keyring.gpg
DEBIAN_SOURCES
  : > "$DOCKER_APT_MAIN_SOURCE_FILE"
  export FAKE_EXPECTED_APT_SOURCELIST="$DOCKER_APT_MAIN_SOURCE_FILE"
  export FAKE_EXPECTED_APT_SOURCEPARTS="$DOCKER_APT_SOURCE_DIR"
  ensure_safe_apt_config
  PACKAGE_LOCK_HELD=1
  MAINTENANCE_READY=1
  FAKE_PACKAGE_LOCKED=1
  export FAKE_PACKAGE_LOCKED
  FAKE_APT_CACHE_MODE=valid
  export FAKE_APT_CACHE_MODE
}

apt_global_config_bypassed_test() (
  prepare_docker_origin_fixture
  local poison="$FAKE_TMP/poison-apt.conf"
  cat > "$poison" <<POISON_APT_CONFIG
Acquire::AllowInsecureRepositories "true";
APT::Get::AllowUnauthenticated "true";
Dir::Etc::sourcelist "$FAKE_TMP/attacker.list";
Dir::Etc::sourceparts "$FAKE_TMP/attacker-parts";
POISON_APT_CONFIG
  export FAKE_POISON_APT_CONFIG="$poison"
  export APT_CONFIG="$poison"
  apt_command apt-get update
  [[ "$APT_CONFIG" == "$SAFE_APT_CONFIG" ]]
)
assert_status 'ambient APT_CONFIG and global apt.conf fragments cannot relax trust or redirect sources' 0 \
  apt_global_config_bypassed_test

valid_docker_origin_test() (
  prepare_docker_origin_fixture
  verify_docker_package_origins "docker-ce=$VELNOR_C1_DOCKER_CE_VERSION"
)
assert_status 'exact Docker candidate comes from the managed Signed-By Trixie suite' 0 \
  valid_docker_origin_test

debian_candidate_origin_test() (
  prepare_docker_origin_fixture
  verify_install_plan_origins --no-upgrade fake-base-package
  awk -F '\t' '$1 == "ACTION" && $2 == "UNPACK" && $3 == "fake-base-package" \
    && $4 == "-" && $5 == "<" && $6 == "1.0-1" { found=1 } END { exit !found }' \
    "$APPROVED_APT_PLAN_FILE"
)
assert_status 'APT simulation verifies every requested base package candidate against active sources' 0 \
  debian_candidate_origin_test

apt_source_input_fingerprint_changes_test() (
  prepare_docker_origin_fixture
  before="$(apt_inputs_fingerprint)"
  printf '\n' >> "$DOCKER_APT_SOURCE_DIR/debian.sources"
  after="$(apt_inputs_fingerprint)"
  [[ "$before" != "$after" ]]
)
assert_status 'APT input fingerprint changes when active source bytes change' 0 \
  apt_source_input_fingerprint_changes_test

base_candidate_attacker_origin_rejected_test() (
  prepare_docker_origin_fixture
  FAKE_APT_CACHE_MODE=wrong-repo
  export FAKE_APT_CACHE_MODE
  verify_install_plan_origins --no-upgrade fake-base-package
)
assert_status 'base package candidate from an unapproved repository is rejected before install' 2 \
  base_candidate_attacker_origin_rejected_test

base_package_dependency_upgrade_rejected_test() (
  prepare_docker_origin_fixture
  APT_UPDATED=1
  FAKE_DEBIAN_PACKAGE_VERSION=2.0-1
  FAKE_APT_SIMULATE_DEP_UPGRADE=1
  printf 'fake-existing-dependency|install ok installed|1.0-1\n' > "$FAKE_DPKG_STATE_FILE"
  export FAKE_DEBIAN_PACKAGE_VERSION FAKE_APT_SIMULATE_DEP_UPGRADE
  : > "$FAKE_TRACE"
  ensure_present_pkgs fake-base-package
)
assert_status 'missing base package resolver refuses to upgrade an installed dependency' 2 \
  base_package_dependency_upgrade_rejected_test
if grep -Fq 'apt-get invocation install -y --no-upgrade --no-remove fake-base-package' "$FAKE_TRACE"; then
  printf 'FAIL dependency upgrade rejection reached the actual package mutation\n' >&2
  exit 1
fi
printf 'PASS dependency upgrade rejection stops before package mutation\n'

apt_simulation_must_include_requested_packages_test() (
  prepare_docker_origin_fixture
  FAKE_APT_SIMULATE_OMIT=1
  export FAKE_APT_SIMULATE_OMIT
  verify_install_plan_origins --no-upgrade fake-base-package
)
assert_status 'APT install plan cannot omit a requested package and still proceed' 2 \
  apt_simulation_must_include_requested_packages_test

write_matching_apt_hook_input() {
  local kind action package old_version direction version
  {
    printf 'VERSION 2\n'
    printf 'Dir::Etc::sourcelist=%s\n' "$DOCKER_APT_MAIN_SOURCE_FILE"
    printf 'Dir::Etc::sourceparts=%s\n' "$DOCKER_APT_SOURCE_DIR"
    printf 'Acquire::AllowInsecureRepositories=false\n'
    printf 'Acquire::AllowDowngradeToInsecureRepositories=false\n'
    printf 'Acquire::AllowWeakRepositories=false\n'
    printf 'APT::Get::AllowUnauthenticated=false\n'
    printf 'Debug::NoLocking=false\n'
    printf 'APT::Architecture=amd64\n\n'
    while IFS=$'\t' read -r kind action package old_version direction version; do
      [[ "$kind" == ACTION ]] || continue
      case "$action" in
        UNPACK)
          printf '%s %s %s %s %s\n' \
            "$package" "$old_version" "$direction" "$version" "$FAKE_ARCHIVE_FILE"
          ;;
        CONFIGURE)
          printf '%s %s %s %s **CONFIGURE**\n' \
            "$package" "$old_version" "$direction" "$version"
          ;;
      esac
    done < "$APPROVED_APT_PLAN_FILE"
  } > "$FAKE_APT_HOOK_INPUT"
}

replace_fixture_once() {
  python3 - "$1" "$2" "$3" <<'REPLACE_FIXTURE'
from pathlib import Path
import sys

path = Path(sys.argv[1])
old, new = sys.argv[2:]
text = path.read_text(encoding="utf-8")
if text.count(old) != 1:
    raise SystemExit(f"expected exactly one fixture occurrence: {old!r}")
path.write_text(text.replace(old, new), encoding="utf-8")
REPLACE_FIXTURE
}

apt_transaction_plan_matches_test() (
  prepare_docker_origin_fixture
  verify_install_plan_origins --no-upgrade --no-remove fake-base-package
  awk -F '\t' '$1 == "META" && $2 == "INPUTS" && $7 == "amd64" { found=1 } END { exit !found }' \
    "$APPROVED_APT_PLAN_FILE"
  FAKE_APT_HOOK_INPUT="$FAKE_TMP/apt-hook-input"
  write_matching_apt_hook_input
  export FAKE_APT_HOOK_INPUT FAKE_APT_RUN_PLAN_HOOK=1
  package_transaction apt_plan_guarded_install install -y --no-upgrade --no-remove fake-base-package
)
assert_status 'fake-host pre-install guard accepts matching actions and archive digest' 0 \
  apt_transaction_plan_matches_test

apt_transaction_candidate_mutation_rejected_test() (
  prepare_docker_origin_fixture
  verify_install_plan_origins --no-upgrade --no-remove fake-base-package
  FAKE_APT_HOOK_INPUT="$FAKE_TMP/apt-hook-input"
  write_matching_apt_hook_input
  FAKE_APT_CACHE_MODE=wrong-repo
  export FAKE_APT_CACHE_MODE
  export FAKE_APT_HOOK_INPUT FAKE_APT_RUN_PLAN_HOOK=1
  package_transaction apt_plan_guarded_install install -y --no-upgrade --no-remove fake-base-package
)
assert_status 'fake-host pre-install guard rejects candidate-origin changes after review' 2 \
  apt_transaction_candidate_mutation_rejected_test

apt_transaction_source_mutation_rejected_test() (
  prepare_docker_origin_fixture
  verify_install_plan_origins --no-upgrade --no-remove fake-base-package
  FAKE_APT_HOOK_INPUT="$FAKE_TMP/apt-hook-input"
  write_matching_apt_hook_input
  printf '\n' >> "$DOCKER_APT_SOURCE_DIR/debian.sources"
  export FAKE_APT_HOOK_INPUT FAKE_APT_RUN_PLAN_HOOK=1
  package_transaction apt_plan_guarded_install install -y --no-upgrade --no-remove fake-base-package
)
assert_status 'fake-host pre-install guard rejects source-file changes after review' 2 \
  apt_transaction_source_mutation_rejected_test

apt_transaction_keyring_mutation_rejected_test() (
  prepare_docker_origin_fixture
  verify_install_plan_origins --no-upgrade --no-remove fake-base-package
  FAKE_APT_HOOK_INPUT="$FAKE_TMP/apt-hook-input"
  write_matching_apt_hook_input
  printf 'rotated key material\n' >> "$DOCKER_APT_KEY_FILE"
  export FAKE_APT_HOOK_INPUT FAKE_APT_RUN_PLAN_HOOK=1
  package_transaction apt_plan_guarded_install install -y --no-upgrade --no-remove fake-base-package
)
assert_status 'fake-host pre-install guard rejects keyring changes after review' 2 \
  apt_transaction_keyring_mutation_rejected_test

apt_transaction_config_mutation_rejected_test() (
  prepare_docker_origin_fixture
  verify_install_plan_origins --no-upgrade --no-remove fake-base-package
  FAKE_APT_HOOK_INPUT="$FAKE_TMP/apt-hook-input"
  write_matching_apt_hook_input
  printf 'APT::Get::AllowUnauthenticated "true";\n' >> "$SAFE_APT_CONFIG"
  export FAKE_APT_HOOK_INPUT FAKE_APT_RUN_PLAN_HOOK=1
  package_transaction apt_plan_guarded_install install -y --no-upgrade --no-remove fake-base-package
)
assert_status 'fake-host pre-install guard rejects a changed private APT config' 2 \
  apt_transaction_config_mutation_rejected_test

apt_transaction_unsafe_hook_config_rejected_test() (
  prepare_docker_origin_fixture
  verify_install_plan_origins --no-upgrade --no-remove fake-base-package
  FAKE_APT_HOOK_INPUT="$FAKE_TMP/apt-hook-input"
  write_matching_apt_hook_input
  replace_fixture_once "$FAKE_APT_HOOK_INPUT" \
    'Acquire::AllowInsecureRepositories=false' 'Acquire::AllowInsecureRepositories=true'
  export FAKE_APT_HOOK_INPUT FAKE_APT_RUN_PLAN_HOOK=1
  package_transaction apt_plan_guarded_install install -y --no-upgrade --no-remove fake-base-package
)
assert_status 'fake-host pre-install guard rejects insecure effective APT options' 2 \
  apt_transaction_unsafe_hook_config_rejected_test

apt_transaction_rejects_config_item_prefix_test() (
  prepare_docker_origin_fixture
  verify_install_plan_origins --no-upgrade --no-remove fake-base-package
  FAKE_APT_HOOK_INPUT="$FAKE_TMP/apt-hook-input"
  write_matching_apt_hook_input
  replace_fixture_once "$FAKE_APT_HOOK_INPUT" \
    'APT::Architecture=amd64' 'Config-Item: APT::Architecture=amd64'
  export FAKE_APT_HOOK_INPUT FAKE_APT_RUN_PLAN_HOOK=1
  package_transaction apt_plan_guarded_install install -y --no-upgrade --no-remove fake-base-package
)
assert_status 'APT v2 guard rejects the obsolete Config-Item fixture protocol' 2 \
  apt_transaction_rejects_config_item_prefix_test

apt_transaction_architecture_mutation_rejected_test() (
  prepare_docker_origin_fixture
  verify_install_plan_origins --no-upgrade --no-remove fake-base-package
  FAKE_APT_HOOK_INPUT="$FAKE_TMP/apt-hook-input"
  write_matching_apt_hook_input
  replace_fixture_once "$FAKE_APT_HOOK_INPUT" \
    'APT::Architecture=amd64' 'APT::Architecture=arm64'
  export FAKE_APT_HOOK_INPUT FAKE_APT_RUN_PLAN_HOOK=1
  package_transaction apt_plan_guarded_install install -y --no-upgrade --no-remove fake-base-package
)
assert_status 'APT pre-install guard binds effective architecture to the resolver plan' 2 \
  apt_transaction_architecture_mutation_rejected_test

apt_transaction_archive_mutation_rejected_test() (
  prepare_docker_origin_fixture
  verify_install_plan_origins --no-upgrade --no-remove fake-base-package
  FAKE_APT_HOOK_INPUT="$FAKE_TMP/apt-hook-input"
  write_matching_apt_hook_input
  printf 'modified package archive bytes\n' > "$FAKE_ARCHIVE_FILE"
  export FAKE_APT_HOOK_INPUT FAKE_APT_RUN_PLAN_HOOK=1
  package_transaction apt_plan_guarded_install install -y --no-upgrade --no-remove fake-base-package
)
assert_status 'fake-host pre-install guard rejects a changed downloaded archive digest' 2 \
  apt_transaction_archive_mutation_rejected_test

apt_transaction_old_version_mutation_rejected_test() (
  prepare_docker_origin_fixture
  verify_install_plan_origins --no-upgrade --no-remove fake-base-package
  FAKE_APT_HOOK_INPUT="$FAKE_TMP/apt-hook-input"
  write_matching_apt_hook_input
  python3 - "$FAKE_APT_HOOK_INPUT" <<'CHANGE_OLD_VERSION'
from pathlib import Path
import sys

path = Path(sys.argv[1])
lines = path.read_text(encoding="utf-8").splitlines()
for index, line in enumerate(lines):
    fields = line.split()
    if fields and fields[0] == "fake-base-package" and fields[-1].endswith(".deb"):
        fields[1] = "0.9-1"
        lines[index] = " ".join(fields)
        break
path.write_text("\n".join(lines) + "\n", encoding="utf-8")
CHANGE_OLD_VERSION
  FAKE_DPKG_COMPARE_GT=1
  export FAKE_DPKG_COMPARE_GT
  export FAKE_APT_HOOK_INPUT FAKE_APT_RUN_PLAN_HOOK=1
  package_transaction apt_plan_guarded_install install -y --no-upgrade --no-remove fake-base-package
)
assert_status 'fake-host pre-install guard rejects an old-version change with same target' 2 \
  apt_transaction_old_version_mutation_rejected_test

apt_transaction_direction_mutation_rejected_test() (
  prepare_docker_origin_fixture
  verify_install_plan_origins --no-upgrade --no-remove fake-base-package
  FAKE_APT_HOOK_INPUT="$FAKE_TMP/apt-hook-input"
  write_matching_apt_hook_input
  python3 - "$FAKE_APT_HOOK_INPUT" <<'CHANGE_DIRECTION'
from pathlib import Path
import sys

path = Path(sys.argv[1])
lines = path.read_text(encoding="utf-8").splitlines()
for index, line in enumerate(lines):
    fields = line.split()
    if fields and fields[0] == "fake-base-package" and fields[-1].endswith(".deb"):
        fields[2] = ">"
        lines[index] = " ".join(fields)
        break
path.write_text("\n".join(lines) + "\n", encoding="utf-8")
CHANGE_DIRECTION
  export FAKE_APT_HOOK_INPUT FAKE_APT_RUN_PLAN_HOOK=1
  package_transaction apt_plan_guarded_install install -y --no-upgrade --no-remove fake-base-package
)
assert_status 'fake-host pre-install guard rejects an upgrade/downgrade direction change' 2 \
  apt_transaction_direction_mutation_rejected_test

apt_transaction_removal_rejected_test() (
  prepare_docker_origin_fixture
  verify_install_plan_origins --no-upgrade --no-remove fake-base-package
  FAKE_APT_HOOK_INPUT="$FAKE_TMP/apt-hook-input"
  write_matching_apt_hook_input
  printf 'fake-conflict 1.0-1 > - **REMOVE**\n' >> "$FAKE_APT_HOOK_INPUT"
  export FAKE_APT_HOOK_INPUT FAKE_APT_RUN_PLAN_HOOK=1
  package_transaction apt_plan_guarded_install install -y --no-upgrade --no-remove fake-base-package
)
assert_status 'fake-host pre-install guard rejects removals despite resolver output' 2 \
  apt_transaction_removal_rejected_test

unapproved_active_debian_source_rejected_test() (
  prepare_docker_origin_fixture
  cat > "$DOCKER_APT_SOURCE_DIR/untrusted.sources" <<'APT_SOURCES'
Types: deb
URIs: https://packages.attacker.invalid/debian
Suites: trixie
Components: main
APT_SOURCES
  preflight_active_apt_sources
)
assert_status 'active deb822 sources outside official repositories are rejected' 2 \
  unapproved_active_debian_source_rejected_test

trusted_list_source_rejected_test() (
  prepare_docker_origin_fixture
  printf 'deb [trusted=yes] https://deb.debian.org/debian trixie main\n' \
    > "$DOCKER_APT_SOURCE_DIR/trusted.list"
  preflight_active_apt_sources
)
assert_status 'APT trusted=yes bypass is rejected from active .list sources' 2 \
  trusted_list_source_rejected_test

candidate_origin_trust_bypass_rejected_test() (
  prepare_docker_origin_fixture
  rm -- "$DOCKER_APT_SOURCE_DIR/debian.sources"
  printf 'deb [trusted=yes] https://deb.debian.org/debian trixie main\n' \
    > "$DOCKER_APT_SOURCE_DIR/trusted.list"
  apt_policy_origin_is_active_source https://deb.debian.org/debian trixie main
)
assert_status 'candidate-origin matcher preserves and rejects fifth-column unsafe-trust metadata' 1 \
  candidate_origin_trust_bypass_rejected_test

docker_key_readability_test() (
  prepare_docker_origin_fixture
  FAKE_STAT_OVERRIDE_PATH="$DOCKER_APT_KEY_FILE"
  FAKE_STAT_MODE_OVERRIDE=644
  assert_status 'world-readable key is usable by _apt' 0 \
    assert_apt_key_readable_by_sandbox "$DOCKER_APT_KEY_FILE"
  FAKE_STAT_OVERRIDE_PATH="$DOCKER_APT_KEY_FILE"
  FAKE_STAT_MODE_OVERRIDE=600
  assert_apt_key_readable_by_sandbox "$DOCKER_APT_KEY_FILE"
)
assert_status 'root-only Docker key mode is rejected because the _apt sandbox cannot read it' 2 \
  docker_key_readability_test

docker_key_parent_permissions_test() (
  prepare_docker_origin_fixture
  FAKE_STAT_OVERRIDE_PATH="${DOCKER_APT_KEY_FILE%/*}"
  FAKE_STAT_MODE_OVERRIDE=700
  assert_apt_key_readable_by_sandbox "$DOCKER_APT_KEY_FILE"
)
assert_status 'non-traversable Docker key parent is rejected for the _apt sandbox' 2 \
  docker_key_parent_permissions_test

docker_key_writable_parent_test() (
  prepare_docker_origin_fixture
  FAKE_STAT_OVERRIDE_PATH="${DOCKER_APT_KEY_FILE%/*}"
  FAKE_STAT_MODE_OVERRIDE=777
  assert_apt_key_readable_by_sandbox "$DOCKER_APT_KEY_FILE"
)
assert_status 'writable Docker key parent is rejected' 2 \
  docker_key_writable_parent_test

for trust_field in Trusted Allow-Insecure; do
  deb822_trust_bypass_rejected_test() (
    prepare_docker_origin_fixture
    cat > "$DOCKER_APT_SOURCE_DIR/trusted.sources" <<APT_SOURCES
Types: deb
URIs: https://deb.debian.org/debian
Suites: trixie
Components: main
$trust_field: yes
APT_SOURCES
    preflight_active_apt_sources
  )
  assert_status "APT Deb822 $trust_field=yes bypass is rejected" 2 \
    deb822_trust_bypass_rejected_test
done

debian_empty_signed_by_rejected_test() (
  prepare_docker_origin_fixture
  printf 'deb https://deb.debian.org/debian trixie main\n' \
    > "$DOCKER_APT_SOURCE_DIR/unbound-debian.list"
  preflight_active_apt_sources
)
assert_status 'Debian source without explicit archive Signed-By is rejected' 2 \
  debian_empty_signed_by_rejected_test

for flat_source in list deb822; do
  flat_apt_source_rejected_test() (
    prepare_docker_origin_fixture
    if [[ "$flat_source" == list ]]; then
      printf 'deb [signed-by=/usr/share/keyrings/debian-archive-keyring.gpg] file:///srv/flat ./\n' \
        > "$DOCKER_APT_SOURCE_DIR/flat.list"
    else
      cat > "$DOCKER_APT_SOURCE_DIR/flat.sources" <<'FLAT_SOURCES'
Types: deb
URIs: file:///srv/flat
Suites: ./
Signed-By: /usr/share/keyrings/debian-archive-keyring.gpg
FLAT_SOURCES
    fi
    preflight_active_apt_sources
  )
  assert_status "active flat ./ APT source in $flat_source form is rejected" 2 \
    flat_apt_source_rejected_test
done

for source_metadata in owner mode hardlink; do
  active_apt_source_metadata_rejected_test() (
    prepare_docker_origin_fixture
    local source="$DOCKER_APT_SOURCE_DIR/debian.sources"
    case "$source_metadata" in
      owner)
        FAKE_STAT_OVERRIDE_PATH="$source"
        FAKE_STAT_OWNER_OVERRIDE=1000:1000
        ;;
      mode)
        FAKE_STAT_OVERRIDE_PATH="$source"
        FAKE_STAT_MODE_OVERRIDE=666
        ;;
      hardlink)
        ln "$source" "$FAKE_APT_DIR/unsafe-source-hardlink"
        ;;
    esac
    preflight_active_apt_sources
  )
  assert_status "active APT source with unsafe $source_metadata is rejected" 2 \
    active_apt_source_metadata_rejected_test
  rm -f -- "$FAKE_APT_DIR/unsafe-source-hardlink"
  FAKE_STAT_OVERRIDE_PATH=
  FAKE_STAT_OWNER_OVERRIDE=
  FAKE_STAT_MODE_OVERRIDE=
done

apt_simulation_removal_rejected_test() (
  prepare_docker_origin_fixture
  FAKE_APT_SIMULATE_REMOVE=1
  export FAKE_APT_SIMULATE_REMOVE
  verify_install_plan_origins --no-upgrade --no-remove fake-base-package
)
assert_status 'APT install plan containing removals is rejected before mutation' 2 \
  apt_simulation_removal_rejected_test

for origin_mode in wrong-candidate wrong-repo wrong-suite duplicate-origin; do
  docker_origin_rejected_test() (
    prepare_docker_origin_fixture
    FAKE_APT_CACHE_MODE="$origin_mode"
    export FAKE_APT_CACHE_MODE
    docker_pin_policy_matches_origin docker-ce "$VELNOR_C1_DOCKER_CE_VERSION"
  )
  assert_status "APT origin policy rejects $origin_mode" 2 docker_origin_rejected_test
done

docker_dot_sources_duplicate_rejected_test() (
  prepare_docker_origin_fixture
  cat > "$DOCKER_APT_SOURCE_DIR/other-docker.sources" <<'APT_SOURCES'
Types: deb
URIs:
 https://download.docker.com/linux/debian
Suites: trixie
Components: stable
APT_SOURCES
  reject_unmanaged_docker_sources "$DOCKER_APT_SOURCE_FILE" \
    "$DOCKER_APT_MAIN_SOURCE_FILE" "$DOCKER_APT_SOURCE_DIR"
)
assert_status 'active Deb822 Docker source cannot bypass managed Signed-By key' 2 \
  docker_dot_sources_duplicate_rejected_test

docker_disabled_alternate_source_ignored_test() (
  prepare_docker_origin_fixture
  cat > "$DOCKER_APT_SOURCE_DIR/disabled-docker.sources" <<'APT_SOURCES'
Types: deb
URIs: https://download.docker.com/linux/debian
Suites: trixie
Components: stable
Enabled: no
APT_SOURCES
  reject_unmanaged_docker_sources "$DOCKER_APT_SOURCE_FILE" \
    "$DOCKER_APT_MAIN_SOURCE_FILE" "$DOCKER_APT_SOURCE_DIR"
)
assert_status 'disabled Deb822 source is not treated as an active Docker source' 0 \
  docker_disabled_alternate_source_ignored_test

host_preflight_rejects_cgroup_v1_test() (
  DOCKER_CGROUP_MARKER_PATH="$FAKE_TMP/no-cgroup-v2/cgroup.controllers"
  DOCKER_DAEMON_CONFIG_PATH="$FAKE_TMP/no-docker-config/daemon.json"
  DOCKER_RUNTIME_UNKNOWN=1
  systemd_available() { return 0; }
  host_has_libvirt_or_qemu() { return 1; }
  preflight_host_invariants
)
assert_status 'cgroup v1 is rejected before package or config changes' 2 \
  host_preflight_rejects_cgroup_v1_test

host_preflight_rejects_wrong_docker_driver_test() (
  DOCKER_CGROUP_MARKER_PATH="$FAKE_TMP/cgroup-v2-marker"
  DOCKER_DAEMON_CONFIG_PATH="$FAKE_TMP/no-docker-config/daemon.json"
  : > "$DOCKER_CGROUP_MARKER_PATH"
  DOCKER_RUNTIME_UNKNOWN=0
  DOCKER_RUNTIME_SNAPSHOT='/var/lib/docker|overlay2|json-file|cgroupfs|2'
  systemd_available() { return 0; }
  host_has_libvirt_or_qemu() { return 1; }
  preflight_host_invariants
)
assert_status 'Docker cgroup driver mismatch is rejected before host changes' 2 \
  host_preflight_rejects_wrong_docker_driver_test

host_preflight_rejects_libvirt_test() (
  DOCKER_CGROUP_MARKER_PATH="$FAKE_TMP/cgroup-v2-marker"
  DOCKER_DAEMON_CONFIG_PATH="$FAKE_TMP/no-docker-config/daemon.json"
  : > "$DOCKER_CGROUP_MARKER_PATH"
  DOCKER_RUNTIME_UNKNOWN=0
  DOCKER_RUNTIME_SNAPSHOT='/var/lib/docker|overlay2|json-file|systemd|2'
  systemd_available() { return 0; }
  host_has_libvirt_or_qemu() { return 0; }
  preflight_host_invariants
)
assert_status 'libvirt or QEMU presence is rejected before host changes' 2 \
  host_preflight_rejects_libvirt_test

host_preflight_allows_confirmed_docker_bootstrap_test() (
  DOCKER_CGROUP_MARKER_PATH="$FAKE_TMP/cgroup-v2-marker"
  DOCKER_DAEMON_CONFIG_PATH="$FAKE_TMP/no-docker-config/daemon.json"
  : > "$DOCKER_CGROUP_MARKER_PATH"
  DOCKER_RUNTIME_UNKNOWN=1
  systemd_available() { return 0; }
  docker_service_execstart() { printf '__absent__\n'; }
  host_has_libvirt_or_qemu() { return 1; }
  preflight_host_invariants
)
assert_status 'cgroup v2 host can bootstrap a confirmed stopped Docker runtime' 0 \
  host_preflight_allows_confirmed_docker_bootstrap_test

host_preflight_rejects_non_systemd_test() (
  DOCKER_CGROUP_MARKER_PATH="$FAKE_TMP/cgroup-v2-marker"
  : > "$DOCKER_CGROUP_MARKER_PATH"
  systemd_available() { return 1; }
  host_has_libvirt_or_qemu() { return 1; }
  preflight_host_invariants
)
assert_status 'non-systemd host is rejected before any package or config mutation' 2 \
  host_preflight_rejects_non_systemd_test

partial_runner_without_lock_is_unknown_test() (
  CHECK=0
  PACKAGE_LOCK_PATH="$FAKE_TMP/partial-runner-no-lock/package-transaction.lock"
  export FAKE_DPKG_STATUS='install ok unpacked'
  preflight_runner_lock
)
assert_status 'partly unpacked runner without a lock cannot be misclassified as absent' 2 \
  partial_runner_without_lock_is_unknown_test

half_configured_runner_without_lock_is_unknown_test() (
  CHECK=0
  PACKAGE_LOCK_PATH="$FAKE_TMP/half-configured-runner-no-lock/package-transaction.lock"
  export FAKE_DPKG_STATUS='install ok half-configured'
  preflight_runner_lock
)
assert_status 'half-configured runner without a lock fails closed' 2 \
  half_configured_runner_without_lock_is_unknown_test

partial_runner_query_failure_is_unknown_test() (
  CHECK=0
  PACKAGE_LOCK_PATH="$FAKE_TMP/partial-runner-query-failure/package-transaction.lock"
  export FAKE_DPKG_STATUS='install ok unpacked' FAKE_DPKG_QUERY_STATUS=1
  preflight_runner_lock
)
assert_status 'failed runner query cannot hide a partial dpkg state' 2 \
  partial_runner_query_failure_is_unknown_test

installed_runner_missing_lock_check_test() (
  CHECK=1
  PACKAGE_LOCK_PATH="$FAKE_TMP/installed-runner-no-lock/package-transaction.lock"
  export FAKE_DPKG_STATUS='install ok installed'
  preflight_runner_lock
)
assert_status '--check reports installed runner with missing lock as unresolved' 2 \
  installed_runner_missing_lock_check_test

apt_hold_check_without_lock_is_unknown_test() (
  CHECK=1
  PACKAGE_LOCK_PATH="$FAKE_TMP/apt-hold-check-no-lock/package-transaction.lock"
  step_holds
)
assert_status '--check preserves unresolved status for missing APT hold lock' 2 \
  apt_hold_check_without_lock_is_unknown_test

FAKE_LOCK_DIR="$FAKE_TMP/run/velnor"
FAKE_LOCK_PATH="$FAKE_LOCK_DIR/package-transaction.lock"
mkdir -p "$FAKE_LOCK_DIR"
: > "$FAKE_LOCK_PATH"

package_lock_helper_test() (
  CHECK=0
  PACKAGE_LOCK_PATH="$FAKE_LOCK_PATH"
  FLOCK_BIN="$FAKE_BIN/flock"
  PACKAGE_LOCK_HELD=0
  MAINTENANCE_READY=0
  FAKE_STAT_OVERRIDE_PATH="$FAKE_LOCK_DIR"
  FAKE_STAT_OWNER_OVERRIDE=0:0
  FAKE_STAT_MODE_OVERRIDE=750
  : > "$FAKE_TRACE"
  ensure_package_lock_file
  chmod 0640 "$FAKE_LOCK_PATH"
  acquire_package_lock
  [[ "$PACKAGE_LOCK_HELD" == 1 ]] || exit 1
  package_lock_mode="$(python3 - "$FAKE_LOCK_PATH" <<'LOCK_MODE_TEST'
import os
import stat
import sys
print(f"{stat.S_IMODE(os.stat(sys.argv[1]).st_mode):03o}")
LOCK_MODE_TEST
)"
  [[ "$package_lock_mode" == 600 ]] || exit 1
  release_package_lock
  FAKE_PACKAGE_LOCKED=0
  export FAKE_PACKAGE_LOCKED
  package_read :
  grep -Fq 'flock exclusive ' "$FAKE_TRACE"
  grep -Fq 'flock shared fd' "$FAKE_TRACE"
  grep -Fq 'flock unlock ' "$FAKE_TRACE"
)
assert_status 'exclusive/shared package locks use an injected temporary lock path' 0 \
  package_lock_helper_test

shared_package_read_waits_for_exclusive_lock_test() (
  PACKAGE_LOCK_PATH="$FAKE_LOCK_PATH"
  FLOCK_BIN="$FAKE_BIN/flock"
  MAINTENANCE_TIMEOUT=30
  printf 'exclusive\n' > "$FAKE_LOCK_STATE_FILE"
  : > "$FAKE_TRACE"
  sleep() {
    printf 'shared wait %s\n' "$1" >> "$FAKE_TRACE"
    : > "$FAKE_LOCK_STATE_FILE"
  }
  package_read_shared :
  grep -Fq 'flock shared blocked fd' "$FAKE_TRACE"
  grep -Fq 'shared wait 1' "$FAKE_TRACE"
  grep -Fq 'flock shared fd' "$FAKE_TRACE"
  grep -Fq 'flock unlock ' "$FAKE_TRACE"
)
assert_status 'shared package reads wait for exclusive APT transactions' 0 \
  shared_package_read_waits_for_exclusive_lock_test

shared_package_read_timeout_test() (
  PACKAGE_LOCK_PATH="$FAKE_LOCK_PATH"
  FLOCK_BIN="$FAKE_BIN/flock"
  MAINTENANCE_TIMEOUT=1
  printf 'exclusive\n' > "$FAKE_LOCK_STATE_FILE"
  trap ' : > "$FAKE_LOCK_STATE_FILE" ' EXIT
  package_read_shared :
)
assert_status 'shared package reads stop waiting at the maintenance timeout' 2 \
  shared_package_read_timeout_test

stock_daemon_unit_validator_test() (
  local unit_root="$FAKE_TMP/vendor-systemd" fake_fragment
  unit_root="$unit_root/usr/lib/systemd/system"
  mkdir -p -- "$unit_root"
  fake_fragment="$unit_root/velnor-daemon.service"
  copy_packaged_daemon_fragment "$fake_fragment"
  SYSTEMD_VENDOR_UNIT_DIRS=("$unit_root")
  # shellcheck disable=SC2329 # assert_stock_daemon_unit resolves this fake host call.
  systemd_available() { return 0; }
  # shellcheck disable=SC2329 # assert_stock_daemon_unit resolves this fake unit metadata.
  systemctl() {
    [[ "$1" == show && "$3" == --value ]] || return 2
    case "$2" in
      --property=LoadState) printf 'loaded\n' ;;
      --property=FragmentPath) printf '%s\n' "$fake_fragment" ;;
      --property=DropInPaths) printf '%s\n' "${FAKE_UNIT_DROPINS:-}" ;;
      --property=ExecStart) printf '%s\n' "${FAKE_UNIT_EXECSTART:-$FAKE_STOCK_DAEMON_EXECSTART}" ;;
      --property=Environment) printf '%s\n' 'VELNOR_STORAGE_ROOT=/var MISE_LOCKFILE=1' ;;
      *) return 2 ;;
    esac
  }
  # shellcheck disable=SC2329 # assert_stock_daemon_unit resolves this metadata validator.
  assert_safe_managed_file() { [[ -f "$1" && ! -L "$1" ]]; }
  assert_stock_daemon_unit velnor-daemon.service velnor-daemon.service /etc/velnor/velnor.env
)
assert_status 'stock daemon validator resolves packaged fragment and environment contract' 0 \
  stock_daemon_unit_validator_test

stock_daemon_template_fragment_validator_test() (
  local unit_root="$FAKE_TMP/vendor-systemd-template" fake_fragment
  mkdir -p -- "$unit_root"
  fake_fragment="$unit_root/velnor-daemon@.service"
  copy_packaged_daemon_fragment "$fake_fragment" velnor-daemon@.service
  SYSTEMD_VENDOR_UNIT_DIRS=("$unit_root")
  systemd_available() { return 0; }
  systemctl() {
    [[ "$1" == show && "$3" == --value ]] || return 2
    case "$2" in
      --property=LoadState) printf 'loaded\n' ;;
      --property=FragmentPath) printf '%s\n' "$fake_fragment" ;;
      --property=DropInPaths) : ;;
      --property=ExecStart) printf '%s\n' "$FAKE_STOCK_DAEMON_EXECSTART" ;;
      --property=Environment) printf '%s\n' 'VELNOR_STORAGE_ROOT=/var MISE_LOCKFILE=1' ;;
      *) return 2 ;;
    esac
  }
  assert_safe_managed_file() { [[ -f "$1" && ! -L "$1" ]]; }
  assert_stock_daemon_unit velnor-daemon@edge.service velnor-daemon@.service \
    /etc/velnor/%i.env
)
assert_status 'daemon template resolves the pinned packaged fragment' 0 \
  stock_daemon_template_fragment_validator_test

stock_daemon_dropin_rejected_test() (
  local unit_root="$FAKE_TMP/vendor-systemd-dropin" fake_fragment
  mkdir -p -- "$unit_root"
  fake_fragment="$unit_root/velnor-daemon.service"
  copy_packaged_daemon_fragment "$fake_fragment"
  SYSTEMD_VENDOR_UNIT_DIRS=("$unit_root")
  systemd_available() { return 0; }
  systemctl() {
    case "$2" in
      --property=LoadState) printf 'loaded\n' ;;
      --property=FragmentPath) printf '%s\n' "$fake_fragment" ;;
      --property=DropInPaths) printf '/etc/systemd/system/velnor-daemon.service.d/override.conf\n' ;;
      --property=ExecStart) printf '%s\n' '/usr/bin/velnor-runner daemon' ;;
      --property=Environment) : ;;
      *) return 2 ;;
    esac
  }
  assert_safe_managed_file() { [[ -f "$1" && ! -L "$1" ]]; }
  assert_stock_daemon_unit velnor-daemon.service velnor-daemon.service /etc/velnor/velnor.env
)
assert_status 'daemon drop-ins are rejected before roster path resolution' 2 \
  stock_daemon_dropin_rejected_test

stock_daemon_custom_storage_flag_rejected_test() (
  local unit_root="$FAKE_TMP/vendor-systemd-storage-flag" fake_fragment
  mkdir -p -- "$unit_root"
  fake_fragment="$unit_root/velnor-daemon.service"
  copy_packaged_daemon_fragment "$fake_fragment"
  SYSTEMD_VENDOR_UNIT_DIRS=("$unit_root")
  systemd_available() { return 0; }
  systemctl() {
    case "$2" in
      --property=LoadState) printf 'loaded\n' ;;
      --property=FragmentPath) printf '%s\n' "$fake_fragment" ;;
      --property=DropInPaths) : ;;
      --property=ExecStart) printf '%s\n' \
        '/usr/bin/flock --shared --no-fork /run/velnor/package-transaction.lock /usr/bin/velnor-runner daemon --url ${VELNOR_URL} --name ${VELNOR_NAME} --labels ${VELNOR_LABELS} --slots ${VELNOR_SLOTS} --work-dir ${VELNOR_WORK_DIR} --state-db /tmp/state.db' ;;
      --property=Environment) : ;;
      *) return 2 ;;
    esac
  }
  assert_safe_managed_file() { [[ -f "$1" && ! -L "$1" ]]; }
  assert_stock_daemon_unit velnor-daemon.service velnor-daemon.service /etc/velnor/velnor.env
)
assert_status 'daemon storage flags outside the environment contract are rejected' 2 \
  stock_daemon_custom_storage_flag_rejected_test

stock_daemon_path_slot_args_must_use_configured_inputs_test() (
  local unit_root="$FAKE_TMP/vendor-systemd-path-slot-args" fake_fragment
  mkdir -p -- "$unit_root"
  fake_fragment="$unit_root/velnor-daemon.service"
  copy_packaged_daemon_fragment "$fake_fragment"
  SYSTEMD_VENDOR_UNIT_DIRS=("$unit_root")
  systemd_available() { return 0; }
  systemctl() {
    case "$2" in
      --property=LoadState) printf 'loaded\n' ;;
      --property=FragmentPath) printf '%s\n' "$fake_fragment" ;;
      --property=DropInPaths) : ;;
      --property=ExecStart) printf '%s\n' \
        '/usr/bin/flock --shared --no-fork /run/velnor/package-transaction.lock /usr/bin/velnor-runner daemon --url https://example.invalid --name wrong --labels ${VELNOR_LABELS} --slots 1 --work-dir /tmp/other --replace' ;;
      --property=Environment) : ;;
      *) return 2 ;;
    esac
  }
  assert_safe_managed_file() { [[ -f "$1" && ! -L "$1" ]]; }
  assert_stock_daemon_unit velnor-daemon.service velnor-daemon.service /etc/velnor/velnor.env
)
assert_status 'effective daemon identity, slot count, and work path must use configured inputs' 2 \
  stock_daemon_path_slot_args_must_use_configured_inputs_test

stock_daemon_environment_cannot_override_roster_inputs_test() (
  local unit_root="$FAKE_TMP/vendor-systemd-roster-env" fake_fragment
  mkdir -p -- "$unit_root"
  fake_fragment="$unit_root/velnor-daemon.service"
  copy_packaged_daemon_fragment "$fake_fragment"
  SYSTEMD_VENDOR_UNIT_DIRS=("$unit_root")
  systemd_available() { return 0; }
  systemctl() {
    case "$2" in
      --property=LoadState) printf 'loaded\n' ;;
      --property=FragmentPath) printf '%s\n' "$fake_fragment" ;;
      --property=DropInPaths) : ;;
      --property=ExecStart) printf '%s\n' "$FAKE_STOCK_DAEMON_EXECSTART" ;;
      --property=Environment) printf 'VELNOR_SLOTS=1\n' ;;
      *) return 2 ;;
    esac
  }
  assert_safe_managed_file() { [[ -f "$1" && ! -L "$1" ]]; }
  assert_stock_daemon_unit velnor-daemon.service velnor-daemon.service /etc/velnor/velnor.env
)
assert_status 'effective unit Environment cannot override roster identity or slot inputs' 2 \
  stock_daemon_environment_cannot_override_roster_inputs_test

stock_daemon_vendor_fragment_tamper_rejected_test() (
  local unit_root="$FAKE_TMP/vendor-systemd-tampered" fake_fragment
  mkdir -p -- "$unit_root"
  fake_fragment="$unit_root/velnor-daemon.service"
  copy_packaged_daemon_fragment "$fake_fragment"
  printf '\nEnvironment=VELNOR_SLOTS=1\n' >> "$fake_fragment"
  SYSTEMD_VENDOR_UNIT_DIRS=("$unit_root")
  systemd_available() { return 0; }
  systemctl() {
    case "$2" in
      --property=LoadState) printf 'loaded\n' ;;
      --property=FragmentPath) printf '%s\n' "$fake_fragment" ;;
      --property=DropInPaths) : ;;
      --property=ExecStart) printf '%s\n' "$FAKE_STOCK_DAEMON_EXECSTART" ;;
      --property=Environment) : ;;
      *) return 2 ;;
    esac
  }
  assert_safe_managed_file() { [[ -f "$1" && ! -L "$1" ]]; }
  assert_stock_daemon_unit velnor-daemon.service velnor-daemon.service /etc/velnor/velnor.env
)
assert_status 'root-owned in-place daemon fragment edits fail the packaged-byte pin' 2 \
  stock_daemon_vendor_fragment_tamper_rejected_test

permit_roster_precedes_admission_reopen_test() (
  local fixture_root="$FAKE_TMP/roster-first-boot" slot index
  mkdir -p -- "$fixture_root/etc/velnor" "$fixture_root/run/velnor" \
    "$fixture_root/run/systemd/system" "$fixture_root/usr/bin"
  printf 'VELNOR_NAME=velnor\nVELNOR_SLOTS=4\n' > "$fixture_root/etc/velnor/velnor.env"
  chmod 0640 "$fixture_root/etc/velnor/velnor.env"
  PACKAGE_LOCK_PATH="$fixture_root/run/velnor/package-transaction.lock"
  : > "$PACKAGE_LOCK_PATH"
  chmod 0600 "$PACKAGE_LOCK_PATH"
  exec 8<> "$PACKAGE_LOCK_PATH"
  python3 -c 'import fcntl, sys; fcntl.flock(int(sys.argv[1]), fcntl.LOCK_EX | fcntl.LOCK_NB)' 8
  PACKAGE_LOCK_FD=8
  cat > "$fixture_root/usr/bin/systemctl" <<'SYSTEMCTL'
#!/bin/sh
[ "${1:-}" = list-units ] || exit 2
exit 0
SYSTEMCTL
  chmod 0755 "$fixture_root/usr/bin/systemctl"
  PERMIT_LEDGER_ROSTER_ROOT="$fixture_root"
  RUNNER_INSTALLED=1
  CHECK=0
  PACKAGE_LOCK_HELD=0
  MAINTENANCE_READY=0
  ADMISSION_UNITS=(velnor-daemon.service)
  DOCKER_CHANGE_STARTED=0
  DOCKER_LOCAL_HEALTH_VERIFIED=1
  local order_file="$FAKE_TMP/roster-first-boot.order"
  : > "$order_file"
  # shellcheck disable=SC2329 # roster step resolves these simulated host controls.
  systemd_available() { return 0; }
  # shellcheck disable=SC2329 # roster step resolves this packaged-unit proof stub.
  assert_stock_daemon_unit() {
    printf 'unit %s\n' "$1" >> "$order_file"
  }
  # shellcheck disable=SC2329 # roster step resolves this isolated unit inventory.
  collect_stock_daemon_instances() { DAEMON_INSTANCES=(); }
  # shellcheck disable=SC2329 # roster step resolves this package-lock barrier.
  ensure_maintenance_barrier() {
    printf 'barrier\n' >> "$order_file"
    PACKAGE_LOCK_HELD=1
    MAINTENANCE_READY=1
  }
  # shellcheck disable=SC2329 # roster step resolves this drain proof stub.
  require_drained() { [[ "$PACKAGE_LOCK_HELD" == 1 ]]; }
  # shellcheck disable=SC2329 # roster step resolves this local helper execution.
  maintenance_command() {
    if [[ "$1" == python3 ]]; then
      [[ "$PACKAGE_LOCK_HELD" == 1 ]] || return 2
      printf 'helper\n' >> "$order_file"
    fi
    "$@"
  }
  # shellcheck disable=SC2329 # restore_admission_units resolves these fake transitions.
  stop_all_active_velnor_units_failsafe() { printf 'stop\n' >> "$order_file"; }
  # shellcheck disable=SC2329 # restore_admission_units resolves this fake lock release.
  release_package_lock() {
    if [[ "$PACKAGE_LOCK_HELD" == 1 && -n "$PACKAGE_LOCK_FD" ]]; then
      printf 'unlock\n' >> "$order_file"
      # The test acquired the real kernel lock through Python's fcntl API.
      # Closing the inherited descriptor releases it without flock(1).
      exec 8>&-
      PACKAGE_LOCK_FD=
      PACKAGE_LOCK_HELD=0
      MAINTENANCE_READY=0
    fi
  }
  # shellcheck disable=SC2329 # restore_admission_units resolves the simulated systemd manager.
  start_admission_units_after_unlock() {
    [[ "$PACKAGE_LOCK_HELD" == 0 ]] || return 2
    [[ -f "$fixture_root/etc/velnor/permit-ledger.sources" ]] || return 2
    [[ -f "$fixture_root/var/lib/velnor/state.db" ]] || return 2
    for index in 1 2 3 4; do
      slot="$fixture_root/var/lib/velnor/runner/daemons/velnor/slots/slot-$index"
      [[ -d "$slot" ]] || return 2
    done
    printf 'start\n' >> "$order_file"
  }
  step_permit_ledger_roster
  [[ "$PACKAGE_LOCK_HELD" == 1 ]]
  [[ -f "$fixture_root/etc/velnor/permit-ledger.sources.lock" ]]
  [[ -f "$fixture_root/var/lib/velnor/permit-ledger.db" ]]
  restore_admission_units
  [[ "$PACKAGE_LOCK_HELD" == 0 && "${#ADMISSION_UNITS[@]}" == 0 ]]
  local barrier_line helper_line unlock_line start_line
  barrier_line="$(grep -n '^barrier$' "$order_file" | cut -d: -f1)"
  helper_line="$(grep -n '^helper$' "$order_file" | cut -d: -f1)"
  unlock_line="$(grep -n '^unlock$' "$order_file" | cut -d: -f1)"
  start_line="$(grep -n '^start$' "$order_file" | cut -d: -f1)"
  (( barrier_line < helper_line && helper_line < unlock_line && unlock_line < start_line ))
)
assert_status 'first boot provisions roster, DB, and four slots under lock before service reopen' 0 \
  permit_roster_precedes_admission_reopen_test

barrier_order_and_restore_test() (
  CHECK=0
  ALLOW_RESTART=0
  MAINTENANCE_TIMEOUT=30
  PACKAGE_LOCK_PATH="$FAKE_LOCK_PATH"
  FLOCK_BIN="$FAKE_BIN/flock"
  PACKAGE_LOCK_HELD=0
  PACKAGE_LOCK_FD=
  MAINTENANCE_READY=0
  ADMISSION_UNITS=()
  ADMISSION_CAPTURED=0
  FAKE_STAT_OVERRIDE_PATH="$FAKE_LOCK_DIR"
  FAKE_STAT_OWNER_OVERRIDE=0:0
  FAKE_STAT_MODE_OVERRIDE=750
  export FAKE_ACTIVE_FILE="$FAKE_TMP/active-units"
  printf 'velnor-daemon.service\nvelnor-daemon.socket\n' > "$FAKE_ACTIVE_FILE"
  export FAKE_REOPEN_ADMISSION_ON_LOCK=1
  : > "$FAKE_TRACE"
  # shellcheck disable=SC2329 # maintenance helpers resolve these fake host calls.
  systemd_available() { return 0; }
  # shellcheck disable=SC2329 # close_velnor_admission resolves this stub.
  active_velnor_units() { cat "$FAKE_ACTIVE_FILE"; }
  # shellcheck disable=SC2329 # maintenance helpers resolve this fake systemctl.
  systemctl() {
    local prefix='' action unit temp skip target
    if [[ "${1:-}" == --no-block ]]; then prefix='--no-block '; shift; fi
    action="$1"
    shift
    printf 'systemctl %s%s %s\n' "$prefix" "$action" "$*" >> "$FAKE_TRACE"
    case "$action" in
      stop)
        temp="$FAKE_ACTIVE_FILE.tmp"
        : > "$temp"
        while IFS= read -r unit; do
          [[ -n "$unit" ]] || continue
          skip=0
          for target in "$@"; do [[ "$unit" == "$target" ]] && skip=1; done
          [[ "$skip" == 1 ]] || printf '%s\n' "$unit" >> "$temp"
        done < "$FAKE_ACTIVE_FILE"
        mv "$temp" "$FAKE_ACTIVE_FILE"
        ;;
      show)
        [[ "${1:-}" == --property=ActiveState && "${2:-}" == --value && -n "${3:-}" ]] || return 2
        if grep -Fqx "$3" "$FAKE_ACTIVE_FILE"; then printf 'active\n'; else printf 'inactive\n'; fi
        ;;
      start)
        [[ "$PACKAGE_LOCK_HELD" == 0 ]] || return 1
        for unit in "$@"; do
          grep -Fqx "$unit" "$FAKE_ACTIVE_FILE" || printf '%s\n' "$unit" >> "$FAKE_ACTIVE_FILE"
        done
        ;;
      restart)
        [[ "$PACKAGE_LOCK_HELD" == 1 ]] || return 1
        ;;
      *) return 2 ;;
    esac
  }
  # shellcheck disable=SC2329 # wait_for_work_drain resolves this fake inventory.
  running_containers() { printf '0\n'; }
  # shellcheck disable=SC2329 # ensure_maintenance_barrier resolves this test trace.
  wait_for_work_drain() { printf 'drain\n' >> "$FAKE_TRACE"; }
  # shellcheck disable=SC2329 # ensure_maintenance_barrier resolves this test trace.
  require_drained() {
    printf 'final-drain\n' >> "$FAKE_TRACE"
    [[ "$PACKAGE_LOCK_HELD" == 1 ]]
  }
  ensure_maintenance_barrier 'fake package/config/restart'
  [[ "${#ADMISSION_UNITS[@]}" == 2 ]]
  restart_docker
  DOCKER_LOCAL_HEALTH_VERIFIED=1
  restore_admission_units
  [[ "$PACKAGE_LOCK_HELD" == 0 && "${#ADMISSION_UNITS[@]}" == 0 ]]
  local stop_one drain_one drain_two drain_three lock_line stop_two final_one final_two restart_line start_line unlock_line drains finals
  local -a drain_lines=() final_lines=()
  stop_one="$(grep -n '^systemctl --no-block stop velnor-daemon.service velnor-daemon.socket$' "$FAKE_TRACE" | cut -d: -f1)"
  drains="$(awk '/^drain$/ { print NR }' "$FAKE_TRACE")"
  mapfile -t drain_lines <<< "$drains"
  [[ "${#drain_lines[@]}" == 3 ]] || return 1
  drain_one="${drain_lines[0]}"
  drain_two="${drain_lines[1]}"
  drain_three="${drain_lines[2]}"
  lock_line="$(grep -n '^flock exclusive ' "$FAKE_TRACE" | cut -d: -f1)"
  stop_two="$(grep -n '^systemctl --no-block stop velnor-guardian.service$' "$FAKE_TRACE" | cut -d: -f1)"
  finals="$(awk '/^final-drain$/ { print NR }' "$FAKE_TRACE")"
  mapfile -t final_lines <<< "$finals"
  [[ "${#final_lines[@]}" == 2 ]] || return 1
  final_one="${final_lines[0]}"
  final_two="${final_lines[1]}"
  restart_line="$(grep -n '^systemctl restart docker$' "$FAKE_TRACE" | cut -d: -f1)"
  start_line="$(grep -n '^systemctl --no-block start velnor-daemon.service velnor-daemon.socket$' "$FAKE_TRACE" | cut -d: -f1)"
  unlock_line="$(grep -n '^flock unlock ' "$FAKE_TRACE" | cut -d: -f1)"
  (( stop_one < drain_one && drain_one < lock_line && lock_line < stop_two \
    && stop_two < drain_two && drain_two < final_one && final_one < drain_three \
    && drain_three < final_two && final_two < restart_line \
    && restart_line < unlock_line && unlock_line < start_line ))
)
assert_status 'barrier closes admission before drain, recloses after lock, and holds through restart/restore' 0 \
  barrier_order_and_restore_test

admission_stop_must_verify_test() (
  CHECK=0
  ADMISSION_UNITS=()
  ADMISSION_CAPTURED=0
  FAKE_ACTIVE_FILE="$FAKE_TMP/stop-did-not-close"
  printf 'velnor-daemon.service\n' > "$FAKE_ACTIVE_FILE"
  systemd_available() { return 0; }
  active_velnor_units() { cat "$FAKE_ACTIVE_FILE"; }
  MAINTENANCE_TIMEOUT=1
  systemctl() {
    if [[ "$1" == --no-block && "$2" == stop ]]; then return 0; fi
    if [[ "$1" == show && "$2" == --property=ActiveState \
      && "$3" == --value && "$4" == velnor-daemon.service ]]; then
      printf 'active\n'
      return 0
    fi
    return 2
  }
  close_velnor_admission
)
assert_status 'maintenance aborts if an admission unit remains active after stop' 2 \
  admission_stop_must_verify_test

wait_drain_rejects_active_containers_test() (
  ALLOW_RESTART=0
  container_count_for_maintenance() { printf '1\n'; }
  wait_for_work_drain
)
assert_status 'drain gate rejects active Docker containers without restart allowance' 2 \
  wait_drain_rejects_active_containers_test

wait_drain_allows_explicit_restart_test() (
  ALLOW_RESTART=1
  container_count_for_maintenance() { printf '1\n'; }
  wait_for_work_drain
)
assert_status 'drain gate allows active containers only with explicit restart allowance' 0 \
  wait_drain_allows_explicit_restart_test

barrier_race_refuses_new_container_test() (
  CHECK=0
  ALLOW_RESTART=0
  MAINTENANCE_TIMEOUT=30
  PACKAGE_LOCK_PATH="$FAKE_LOCK_PATH"
  FLOCK_BIN="$FAKE_BIN/flock"
  PACKAGE_LOCK_HELD=0
  PACKAGE_LOCK_FD=
  MAINTENANCE_READY=0
  ADMISSION_UNITS=()
  FAKE_STAT_OVERRIDE_PATH="$FAKE_LOCK_DIR"
  FAKE_STAT_OWNER_OVERRIDE=0:0
  FAKE_STAT_MODE_OVERRIDE=750
  export FAKE_ACTIVE_FILE="$FAKE_TMP/race-units"
  export FAKE_REOPEN_ADMISSION_ON_LOCK=1
  printf 'velnor-daemon.service\n' > "$FAKE_ACTIVE_FILE"
  export FAKE_COUNT_INDEX="$FAKE_TMP/container-index"
  export FAKE_CONTAINER_SEQUENCE='0 1'
  printf '0\n' > "$FAKE_COUNT_INDEX"
  : > "$FAKE_TRACE"
  # shellcheck disable=SC2329 # barrier helpers resolve only fake host calls.
  systemd_available() { return 0; }
  active_velnor_units() { cat "$FAKE_ACTIVE_FILE"; }
  systemctl() {
    if [[ "$1" == --no-block && "$2" == stop ]]; then
      printf 'systemctl --no-block stop %s\n' "${*:3}" >> "$FAKE_TRACE"
      : > "$FAKE_ACTIVE_FILE"
      return 0
    fi
    if [[ "$1" == show && "$2" == --property=ActiveState \
      && "$3" == --value ]]; then
      printf 'inactive\n'
      return 0
    fi
    return 2
  }
  running_containers() {
    local index count
    local -a values=()
    index="$(<"$FAKE_COUNT_INDEX")"
    read -r -a values <<< "$FAKE_CONTAINER_SEQUENCE"
    if (( index < ${#values[@]} )); then count="${values[$index]}"; else count="${values[${#values[@]}-1]}"; fi
    printf '%s\n' "$((index + 1))" > "$FAKE_COUNT_INDEX"
    printf 'containers %s\n' "$count" >> "$FAKE_TRACE"
    printf '%s\n' "$count"
  }
  package_transaction apt-get install -y race-test=1
)
: > "$FAKE_TRACE"
assert_status 'barrier catches admission/container race before package mutation' 2 \
  barrier_race_refuses_new_container_test
for expected in \
  'containers 0' \
  'containers 1' \
  'systemctl --no-block stop velnor-daemon.service'; do
  if ! grep -Fqx -- "$expected" "$FAKE_TRACE"; then
    printf 'FAIL barrier race trace is missing: %s\n' "$expected" >&2
    cat "$FAKE_TRACE" >&2
    exit 1
  fi
done
if grep -Eq '^apt-get ' "$FAKE_TRACE"; then
  printf 'FAIL raced maintenance reached a package transaction\n' >&2
  exit 1
fi

cleanup_failure_restores_prestate_test() (
  CHECK=0
  ADMISSION_UNITS=(velnor-daemon.service velnor-daemon.socket)
  PACKAGE_LOCK_HELD=1
  CLEANUP_PATHS=()
  FAKE_ACTIVE_FILE="$FAKE_TMP/cleanup-failure-active-units"
  : > "$FAKE_ACTIVE_FILE"
  : > "$FAKE_TRACE"
  # shellcheck disable=SC2329 # cleanup resolves this fake systemd call.
  systemd_available() { return 0; }
  active_velnor_units() { cat "$FAKE_ACTIVE_FILE"; }
  systemctl() {
    local action="$1"
    shift
    if [[ "$action" == --no-block ]]; then
      action="$1"
      shift
      [[ "$action" == start && "$PACKAGE_LOCK_HELD" == 0 ]] || return 1
      printf 'systemctl --no-block start %s\n' "$*" >> "$FAKE_TRACE"
      printf '%s\n' "$@" >> "$FAKE_ACTIVE_FILE"
    elif [[ "$action" == show && "${1:-}" == --property=ActiveState \
      && "${2:-}" == --value && -n "${3:-}" ]]; then
      if grep -Fqx "$3" "$FAKE_ACTIVE_FILE"; then printf 'active\n'; else printf 'inactive\n'; fi
    else
      return 1
    fi
  }
  release_package_lock() {
    if [[ "$PACKAGE_LOCK_HELD" == 1 ]]; then
      printf 'unlock\n' >> "$FAKE_TRACE"
      PACKAGE_LOCK_HELD=0
    fi
  }
  trap cleanup EXIT
  exit 17
)
assert_status 'failed apply restores prior Velnor units and keeps failure status' 17 \
  cleanup_failure_restores_prestate_test
expected_rollback=$'unlock\nsystemctl --no-block start velnor-daemon.service velnor-daemon.socket'
assert_equal 'failure rollback releases the lock before starting shared-lock services' \
  "$expected_rollback" "$(<"$FAKE_TRACE")"

FAKE_PACKAGE_LOCKED=0
export FAKE_PACKAGE_LOCKED
pkg_installed() {
  [[ "${1:-}" == velnor-runner ]]
}
ensure_maintenance_barrier() {
  [[ "$MAINTENANCE_READY" == 1 ]] && return 0
  printf 'maintenance barrier\n' >> "$FAKE_TRACE"
  PACKAGE_LOCK_HELD=1
  MAINTENANCE_READY=1
  FAKE_PACKAGE_LOCKED=1
  export FAKE_PACKAGE_LOCKED
}
require_drained() {
  [[ "$PACKAGE_LOCK_HELD" == 1 ]]
}
apt_update_once() {
  package_transaction apt-get update
}

package_transaction_test() (
  CHECK=0
  PACKAGE_LOCK_HELD=0
  PACKAGE_LOCK_FD=
  MAINTENANCE_READY=0
  ADMISSION_UNITS=()
  APT_UPDATED=0
  prepare_docker_origin_fixture
  printf 'docker-ce\ncontainerd.io\n' > "$FAKE_HOLDS_FILE"
  : > "$FAKE_TRACE"
  step_docker_packages >/dev/null
  expected_holds=$'containerd.io\ndocker-buildx-plugin\ndocker-ce\ndocker-ce-cli\ndocker-compose-plugin'
  actual_holds="$(sort "$FAKE_HOLDS_FILE")"
  [[ "$actual_holds" == "$expected_holds" ]]
  grep -Fqx 'unhold docker-ce' "$FAKE_TRACE"
  grep -Fqx 'unhold containerd.io' "$FAKE_TRACE"
  grep -Fq 'apt-get update' "$FAKE_TRACE"
  grep -Fq 'apt-get invocation install -y --no-remove docker-ce=' "$FAKE_TRACE"
  ensure_present_pkgs fake-base-package
  grep -Fq 'apt-get invocation install -y --no-upgrade --no-remove fake-base-package' "$FAKE_TRACE"
  step_holds >/dev/null
  grep -Fqx 'hold velnor-runner' "$FAKE_TRACE"
)
assert_status 'APT installs and apt-mark hold transactions require lock marker' 0 \
  package_transaction_test

docker_inline_install_plan_hook_success_test() (
  CHECK=0
  PACKAGE_LOCK_HELD=0
  PACKAGE_LOCK_FD=
  MAINTENANCE_READY=0
  MAINTENANCE_DEADLINE=0
  ADMISSION_UNITS=()
  APT_UPDATED=1
  FAKE_DPKG_COMPARE_GT=0
  FAKE_APT_RUN_PLAN_HOOK=1
  FAKE_APT_GENERATE_HOOK_INPUT=1
  FAKE_APT_HOOK_INPUT="$FAKE_TMP/docker-hook-input"
  export FAKE_DPKG_COMPARE_GT FAKE_APT_RUN_PLAN_HOOK
  export FAKE_APT_GENERATE_HOOK_INPUT FAKE_APT_HOOK_INPUT
  prepare_docker_origin_fixture
  : > "$FAKE_DPKG_STATE_FILE"
  : > "$FAKE_HOLDS_FILE"
  for spec in \
    "docker-ce=$VELNOR_C1_DOCKER_CE_VERSION" \
    "docker-ce-cli=$VELNOR_C1_DOCKER_CE_CLI_VERSION" \
    "containerd.io=$VELNOR_C1_CONTAINERD_VERSION" \
    "docker-buildx-plugin=$VELNOR_C1_BUILDX_VERSION" \
    "docker-compose-plugin=$VELNOR_C1_COMPOSE_VERSION"; do
    printf '%s|install ok installed|1.0-1\n' "${spec%%=*}" >> "$FAKE_DPKG_STATE_FILE"
  done
  : > "$FAKE_TRACE"
  step_docker_packages >/dev/null || {
    status=$?
    cat "$FAKE_TRACE" >&2
    exit "$status"
  }
  grep -Fq 'apt-get invocation install -y --no-remove docker-ce=' "$FAKE_TRACE"
  grep -Fq 'DPkg::Pre-Install-Pkgs::=' "$FAKE_TRACE"
  grep -Fq 'APT transaction actions and source/config/origin/archive evidence match' "$FAKE_TRACE"
  grep -Fq 'ACTION	UNPACK	docker-ce	1.0-1	<	' "$APPROVED_APT_PLAN_FILE"
)
assert_status 'C3 inline Docker install reaches the evidence-checking pre-install hook' 0 \
  docker_inline_install_plan_hook_success_test

docker_inline_install_hook_rejects_candidate_change_test() (
  CHECK=0
  PACKAGE_LOCK_HELD=0
  PACKAGE_LOCK_FD=
  MAINTENANCE_READY=0
  MAINTENANCE_DEADLINE=0
  ADMISSION_UNITS=()
  APT_UPDATED=1
  FAKE_DPKG_COMPARE_GT=0
  FAKE_APT_RUN_PLAN_HOOK=1
  FAKE_APT_GENERATE_HOOK_INPUT=1
  FAKE_APT_HOOK_INPUT="$FAKE_TMP/docker-hook-input-mutated"
  export FAKE_DPKG_COMPARE_GT FAKE_APT_RUN_PLAN_HOOK
  export FAKE_APT_GENERATE_HOOK_INPUT FAKE_APT_HOOK_INPUT
  prepare_docker_origin_fixture
  : > "$FAKE_DPKG_STATE_FILE"
  printf 'docker-ce\ncontainerd.io\ndocker-ce-cli\ndocker-buildx-plugin\ndocker-compose-plugin\n' \
    > "$FAKE_HOLDS_FILE"
  for spec in \
    "docker-ce=$VELNOR_C1_DOCKER_CE_VERSION" \
    "docker-ce-cli=$VELNOR_C1_DOCKER_CE_CLI_VERSION" \
    "containerd.io=$VELNOR_C1_CONTAINERD_VERSION" \
    "docker-buildx-plugin=$VELNOR_C1_BUILDX_VERSION" \
    "docker-compose-plugin=$VELNOR_C1_COMPOSE_VERSION"; do
    printf '%s|install ok installed|1.0-1\n' "${spec%%=*}" >> "$FAKE_DPKG_STATE_FILE"
  done
  local before_state before_holds
  before_state="$(<"$FAKE_DPKG_STATE_FILE")"
  before_holds="$(LC_ALL=C sort "$FAKE_HOLDS_FILE")"
  require_drained() {
    if [[ "$1" == *'immediately before package transaction' ]]; then
      FAKE_APT_CACHE_MODE=wrong-repo
      export FAKE_APT_CACHE_MODE
    fi
    [[ "$PACKAGE_LOCK_HELD" == 1 ]]
  }
  : > "$FAKE_TRACE"
  if step_docker_packages >/dev/null 2>&1; then return 1; else status=$?; fi
  [[ "$status" == 2 ]]
  [[ "$(<"$FAKE_DPKG_STATE_FILE")" == "$before_state" ]]
  [[ "$(LC_ALL=C sort "$FAKE_HOLDS_FILE")" == "$before_holds" ]]
  grep -Fq 'APT candidate origin evidence changed' "$FAKE_TRACE"
)
assert_status 'C3 hook rejects candidate-origin drift before dpkg changes package state' 0 \
  docker_inline_install_hook_rejects_candidate_change_test

docker_older_pinned_versions_upgrade_test() (
  CHECK=0
  PACKAGE_LOCK_HELD=0
  PACKAGE_LOCK_FD=
  MAINTENANCE_READY=0
  ADMISSION_UNITS=()
  APT_UPDATED=0
  FAKE_DPKG_COMPARE_GT=0
  export FAKE_DPKG_COMPARE_GT
  prepare_docker_origin_fixture
  : > "$FAKE_DPKG_STATE_FILE"
  for spec in \
    "docker-ce=$VELNOR_C1_DOCKER_CE_VERSION" \
    "docker-ce-cli=$VELNOR_C1_DOCKER_CE_CLI_VERSION" \
    "containerd.io=$VELNOR_C1_CONTAINERD_VERSION" \
    "docker-buildx-plugin=$VELNOR_C1_BUILDX_VERSION" \
    "docker-compose-plugin=$VELNOR_C1_COMPOSE_VERSION"; do
    printf '%s|install ok installed|1.0-1\n' "${spec%%=*}" >> "$FAKE_DPKG_STATE_FILE"
  done
  : > "$FAKE_TRACE"
  step_docker_packages >/dev/null
  expected_install="apt-get invocation install -y --no-remove \
docker-ce=$VELNOR_C1_DOCKER_CE_VERSION \
docker-ce-cli=$VELNOR_C1_DOCKER_CE_CLI_VERSION \
containerd.io=$VELNOR_C1_CONTAINERD_VERSION \
docker-buildx-plugin=$VELNOR_C1_BUILDX_VERSION \
docker-compose-plugin=$VELNOR_C1_COMPOSE_VERSION"
  grep -Fqx "$expected_install" "$FAKE_TRACE"
)
assert_status 'older installed Docker packages converge to every exact pin' 0 \
  docker_older_pinned_versions_upgrade_test

docker_newer_installed_pin_rejected_test() (
  CHECK=0
  PACKAGE_LOCK_HELD=0
  PACKAGE_LOCK_FD=
  MAINTENANCE_READY=0
  ADMISSION_UNITS=()
  APT_UPDATED=0
  FAKE_DPKG_COMPARE_GT=1
  export FAKE_DPKG_COMPARE_GT
  prepare_docker_origin_fixture
  printf 'docker-ce|install ok installed|99.0-1\n' > "$FAKE_DPKG_STATE_FILE"
  : > "$FAKE_TRACE"
  trap 'if grep -Eq "^(apt-get update|unhold |hold )" "$FAKE_TRACE"; then exit 3; fi' EXIT
  step_docker_packages >/dev/null
)
assert_status 'newer installed Docker version fails closed before mutation' 2 \
  docker_newer_installed_pin_rejected_test

apt_update_failure_stops_before_package_mutation_test() (
  CHECK=0
  PACKAGE_LOCK_HELD=0
  PACKAGE_LOCK_FD=
  MAINTENANCE_READY=0
  ADMISSION_UNITS=()
  APT_UPDATED=0
  prepare_docker_origin_fixture
  FAKE_FAIL_APT_UPDATE="$FAKE_TMP/fail-apt-update"
  touch "$FAKE_FAIL_APT_UPDATE"
  export FAKE_FAIL_APT_UPDATE
  : > "$FAKE_TRACE"
  step_docker_packages
)
assert_status 'APT index update failure stops before package or hold mutations' 41 \
  apt_update_failure_stops_before_package_mutation_test
if grep -Eq '^(unhold|hold |apt-get invocation install )' "$FAKE_TRACE"; then
  printf 'FAIL APT update failure reached a package or hold mutation\n' >&2
  exit 1
fi

partial_apt_install_failure_reholds_installed_packages_test() (
  local before_state status expected_holds
  CHECK=0
  PACKAGE_LOCK_HELD=0
  PACKAGE_LOCK_FD=
  MAINTENANCE_READY=0
  ADMISSION_UNITS=()
  APT_UPDATED=1
  prepare_docker_origin_fixture
  printf 'docker-ce\ncontainerd.io\n' > "$FAKE_HOLDS_FILE"
  FAKE_FAIL_APT_INSTALL="$FAKE_TMP/fail-apt-install"
  FAKE_APT_PARTIAL_INSTALL=1
  touch "$FAKE_FAIL_APT_INSTALL"
  export FAKE_FAIL_APT_INSTALL FAKE_APT_PARTIAL_INSTALL
  : > "$FAKE_TRACE"
  before_state="$(<"$FAKE_DPKG_STATE_FILE")"
  if step_docker_packages; then return 1; else status=$?; fi
  [[ "$status" == 42 ]]
  [[ -z "$before_state" ]]
  expected_holds="$(printf 'containerd.io\ndocker-ce\ndocker-ce-cli\n' | LC_ALL=C sort)"
  [[ "$(LC_ALL=C sort "$FAKE_HOLDS_FILE")" == "$expected_holds" ]]
  return "$status"
)
partial_holds_expected="$(printf 'containerd.io\ndocker-ce\ndocker-ce-cli\n' | LC_ALL=C sort)"
assert_status 'partial Docker install failure returns the package error after rollback' 42 \
  partial_apt_install_failure_reholds_installed_packages_test
for partial_pkg in docker-ce containerd.io; do
  grep -Fqx "$partial_pkg" "$FAKE_HOLDS_FILE" \
    || { printf 'FAIL partial transaction did not restore hold for %s\n' "$partial_pkg" >&2; exit 1; }
done
grep -Fq 'apt-get invocation install -y --no-remove docker-ce=' "$FAKE_TRACE"
printf 'PASS partial Docker install failure restores holds for packages left installed\n'
assert_equal 'partial apt rollback keeps all pre-existing holds' \
  "$partial_holds_expected" "$(LC_ALL=C sort "$FAKE_HOLDS_FILE")"

docker_bootstrap_from_absent_test() (
  CHECK=0
  WORK_INVENTORY_UNKNOWN=0
  DOCKER_SOCKET_PATHS=("$FAKE_TMP/no-docker.sock")
  PACKAGE_LOCK_PATH="$FAKE_LOCK_PATH"
  FLOCK_BIN="$FAKE_BIN/flock"
  PACKAGE_LOCK_HELD=0
  MAINTENANCE_READY=0
  ADMISSION_UNITS=()
  ADMISSION_CAPTURED=0
  APT_UPDATED=0
  FAKE_DOCKER_HEALTH=0
  FAKE_SERVICE_ENABLED=0
  FAKE_SERVICE_ACTIVE=0
  FAKE_PACKAGE_LOCKED=0
  export FAKE_PACKAGE_LOCKED
  prepare_docker_origin_fixture
  : > "$FAKE_TRACE"
  systemd_available() { return 0; }
  pgrep() { return 1; }
  systemctl() {
    local action="$1"
    shift
    case "$action" in
      show)
        case "${1:-}" in
          --property=LoadState) printf 'not-found\n' ;;
          --property=ActiveState) printf 'inactive\n' ;;
          *) return 2 ;;
        esac
        ;;
      list-units|list-sockets) : ;;
      is-enabled) [[ "$FAKE_SERVICE_ENABLED" == 1 ]] ;;
      is-active) [[ "$FAKE_SERVICE_ACTIVE" == 1 ]] ;;
      enable)
        FAKE_SERVICE_ENABLED=1
        printf 'systemctl enable docker\n' >> "$FAKE_TRACE"
        ;;
      start)
        [[ "${1:-}" == docker ]] || return 2
        FAKE_SERVICE_ACTIVE=1
        FAKE_DOCKER_HEALTH=1
        printf 'systemctl start docker\n' >> "$FAKE_TRACE"
        ;;
      *) return 2 ;;
    esac
  }
  active_velnor_units() { :; }
  ensure_maintenance_barrier() {
    [[ "$MAINTENANCE_READY" == 1 ]] && return 0
    printf 'maintenance barrier\n' >> "$FAKE_TRACE"
    PACKAGE_LOCK_HELD=1
    MAINTENANCE_READY=1
    FAKE_PACKAGE_LOCKED=1
    export FAKE_PACKAGE_LOCKED
  }
  require_drained() { [[ "$PACKAGE_LOCK_HELD" == 1 ]]; }
  docker_info_snapshot() {
    if [[ "$FAKE_DOCKER_HEALTH" == 1 ]]; then
      printf 'docker info healthy\n' >> "$FAKE_TRACE"
      printf '/var/lib/docker|overlay2|json-file|systemd|2\n'
    else
      printf 'docker info unavailable\n' >> "$FAKE_TRACE"
      return 2
    fi
  }
  preflight_work
  preflight_docker_runtime
  step_docker_packages >/dev/null
  step_service >/dev/null
  require_docker_info_snapshot >/dev/null
  local missing_line install_line start_line healthy_line
  missing_line="$(grep -n '^docker info unavailable$' "$FAKE_TRACE" | cut -d: -f1)"
  install_line="$(grep -n '^apt-get invocation install -y --no-remove docker-ce=' "$FAKE_TRACE" | cut -d: -f1)"
  start_line="$(grep -n '^systemctl start docker$' "$FAKE_TRACE" | cut -d: -f1)"
  healthy_line="$(grep -n '^docker info healthy$' "$FAKE_TRACE" | cut -d: -f1)"
  (( missing_line < install_line && install_line < start_line && start_line < healthy_line ))
)
assert_status 'absent Docker bootstraps through pinned install, service start, and health check' 0 \
  docker_bootstrap_from_absent_test

missing_runner_lock_repair_test() (
  prepare_docker_origin_fixture
  CHECK=0
  VELNOR_LOCK_MISSING=0
  PACKAGE_LOCK_HELD=0
  PACKAGE_LOCK_FD=
  MAINTENANCE_READY=0
  ADMISSION_UNITS=()
  ADMISSION_CAPTURED=0
  PACKAGE_LOCK_PATH="$FAKE_TMP/missing-runner-lock/package-transaction.lock"
  FLOCK_BIN="$FAKE_BIN/flock"
  FAKE_STAT_OVERRIDE_PATH="${PACKAGE_LOCK_PATH%/*}"
  FAKE_STAT_OWNER_OVERRIDE=0:0
  FAKE_STAT_MODE_OVERRIDE=750
  FAKE_PACKAGE_LOCKED=0
  PACKAGE_LOCK_HELD=0
  PACKAGE_LOCK_FD=
  MAINTENANCE_READY=0
  ADMISSION_UNITS=()
  ADMISSION_CAPTURED=0
  export FAKE_PACKAGE_LOCKED
  systemd_available() { return 0; }
  active_velnor_units() { :; }
  export FAKE_DPKG_STATUS='install ok installed'
  running_containers() { printf '0\n'; }
  close_velnor_admission() { printf 'admission closed\n' >> "$FAKE_TRACE"; }
  wait_for_work_drain() { printf 'work drained\n' >> "$FAKE_TRACE"; }
  require_drained() {
    printf 'final drain\n' >> "$FAKE_TRACE"
    [[ "$PACKAGE_LOCK_HELD" == 1 ]]
  }
  install() {
    [[ "$1" == -d ]] || return 2
    local target=
    for target in "$@"; do :; done
    mkdir -p -- "$target"
    chmod 0750 -- "$target"
  }
  chown() { printf 'chown %s\n' "$*" >> "$FAKE_TRACE"; }
  ensure_maintenance_barrier() {
    [[ "$MAINTENANCE_READY" == 1 ]] && return 0
    MAINTENANCE_DEADLINE=$((SECONDS + MAINTENANCE_TIMEOUT))
    close_velnor_admission
    wait_for_work_drain
    acquire_package_lock
    close_velnor_admission
    wait_for_work_drain
    require_drained 'repair the missing Velnor package transaction lock'
    MAINTENANCE_READY=1
  }
  : > "$FAKE_LOCK_STATE_FILE"
  : > "$FAKE_FLOCK_BUSY_FILE"
  : > "$FAKE_TRACE"
  preflight_runner_lock
  [[ "$VELNOR_LOCK_MISSING" == 1 ]]
  package_read apt-mark showhold >/dev/null
  [[ -f "$PACKAGE_LOCK_PATH" ]]
  actual_runtime_mode="$(python3 - "${PACKAGE_LOCK_PATH%/*}" <<'MODE_TEST'
import os
import stat
import sys
print(f"{stat.S_IMODE(os.lstat(sys.argv[1]).st_mode):04o}")
MODE_TEST
)"
  [[ "$actual_runtime_mode" == 0750 ]]
  actual_lock_mode="$(python3 - "$PACKAGE_LOCK_PATH" <<'MODE_TEST'
import os
import stat
import sys
print(f"{stat.S_IMODE(os.lstat(sys.argv[1]).st_mode):04o}")
MODE_TEST
)"
  [[ "$actual_lock_mode" == 0600 ]]
  admission_line="$(awk '/^admission closed$/ { print NR; exit }' "$FAKE_TRACE")"
  drain_line="$(awk '/^work drained$/ { print NR; exit }' "$FAKE_TRACE")"
  lock_line="$(awk '/^flock exclusive / { print NR; exit }' "$FAKE_TRACE")"
  final_drain_line="$(awk '/^final drain$/ { line = NR } END { if (line) print line }' "$FAKE_TRACE")"
  query_line="$(awk '/^apt-mark showhold$/ { print NR; exit }' "$FAKE_TRACE")"
  (( admission_line < drain_line && drain_line < lock_line \
    && lock_line < final_drain_line && final_drain_line < query_line ))
  release_package_lock
)
assert_status 'missing runner lock is repaired mode 0600 and queried under the barrier' 0 \
  missing_runner_lock_repair_test

missing_lock_check_is_unknown_test() (
  CHECK=1
  PACKAGE_LOCK_PATH="$FAKE_TMP/no-package-lock"
  package_read apt-mark showhold
)
assert_status '--check treats missing package lock as unresolved APT state' 2 \
  missing_lock_check_is_unknown_test

apt_check_unresolved_test() (
  CHECK=1
  pkg_installed() { return 1; }
  step_docker_packages
)
: > "$FAKE_TRACE"
if apt_check_unresolved_test > "$FAKE_TMP/apt-check.out" 2>&1; then
  check_status=0
else
  check_status=$?
fi
assert_equal '--check refuses to claim unresolved exact Docker pins' 2 "$check_status"
grep -Fq 'APT resolution unresolved in --check for exact Docker pins' "$FAKE_TMP/apt-check.out"
[[ ! -s "$FAKE_TRACE" ]] \
  || { printf 'FAIL unresolved --check ran an APT command\n' >&2; exit 1; }
printf 'PASS --check reports exact pinned APT resolution as unresolved\n'

docker_package_query_failure_test() (
  CHECK=0
  PACKAGE_LOCK_HELD=0
  PACKAGE_LOCK_FD=
  MAINTENANCE_READY=0
  ADMISSION_UNITS=()
  APT_UPDATED=1
  ensure_maintenance_barrier() {
    PACKAGE_LOCK_HELD=1
    MAINTENANCE_READY=1
    FAKE_PACKAGE_LOCKED=1
    export FAKE_PACKAGE_LOCKED
  }
  require_drained() { return 0; }
  apt_update_once() { :; }
  pkg_installed() { return 1; }
  prepare_docker_origin_fixture
  : > "$FAKE_TRACE"
  touch "$FAKE_FAIL_SHOWHOLD"
  step_docker_packages
)
holds_before="$(cat "$FAKE_HOLDS_FILE")"
assert_status 'Docker install aborts when apt-mark showhold fails' 2 \
  docker_package_query_failure_test
assert_equal 'failed Docker hold query preserves holds' "$holds_before" "$(cat "$FAKE_HOLDS_FILE")"
if grep -Eq '^(unhold |hold |apt-get invocation (install|update) )' "$FAKE_TRACE"; then
  printf 'FAIL hold-query failure reached a package mutation\n' >&2
  exit 1
fi
grep -Fq 'apt-get invocation --simulate' "$FAKE_TRACE" \
  || { printf 'FAIL hold-query failure skipped read-only plan resolution\n' >&2; exit 1; }

step_holds_failure_guard() (
  PACKAGE_LOCK_HELD=1
  FAKE_PACKAGE_LOCKED=1
  export FAKE_PACKAGE_LOCKED
  step_holds
)
assert_status 'hold reconciliation aborts when apt-mark showhold fails' 1 \
  step_holds_failure_guard
assert_equal 'failed hold queries preserve existing package holds' \
  "$holds_before" "$(cat "$FAKE_HOLDS_FILE")"
if grep -Eq '^(unhold |hold |apt-get invocation (install|update) )' "$FAKE_TRACE"; then
  printf 'FAIL hold-query failure reached a package mutation\n' >&2
  exit 1
fi
rm -f -- "$FAKE_FAIL_SHOWHOLD"
printf 'PASS hold-query failures stop before package mutation and preserve holds\n'

FAKE_DEPLOY_HOST=c1.fake.invalid
FAKE_KNOWN_HOSTS="$FAKE_TMP/known_hosts"
printf '%s ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIFAKEKEYFORLOCALTESTONLY\n' \
  "$FAKE_DEPLOY_HOST" > "$FAKE_KNOWN_HOSTS"

cat > "$FAKE_BIN/ssh-keygen" <<'FAKE_SSH_KEYGEN'
#!/usr/bin/env bash
set -euo pipefail
[[ "$1" == -F && "$3" == -f && -s "$4" ]] || exit 2
grep -Fq "$2" "$4"
FAKE_SSH_KEYGEN

cat > "$FAKE_BIN/ssh" <<'FAKE_SSH'
#!/usr/bin/env bash
set -euo pipefail
while [[ "${1:-}" == -o ]]; do shift 2; done
target="${1:-}"
shift
[[ "$target" == "root@$FAKE_DEPLOY_HOST" ]] || exit 2
if [[ "${1:-}" == 'mktemp -d /root/.c1-host-setup.stage.XXXXXX' ]]; then
  stage="$FAKE_REMOTE_ROOT/.c1-host-setup.stage.ABC123"
  mkdir -p "$stage"
  printf '/root/.c1-host-setup.stage.ABC123\n'
  exit 0
fi
[[ "${1:-}" == bash && "${2:-}" == -s && "${3:-}" == -- ]] || exit 2
shift 3
remote_script="$(mktemp)"
cat > "$remote_script"
if [[ "$#" == 3 ]]; then
  digest="$1"
  stage="${2#/root/}"
  stage="$FAKE_REMOTE_ROOT/$stage"
  if bash "$remote_script" "$digest" "$stage" "$FAKE_REMOTE_ROOT"; then status=0; else status=$?; fi
elif [[ "$#" == 2 ]]; then
  stage="${1#/root/}"
  stage="$FAKE_REMOTE_ROOT/$stage"
  if bash "$remote_script" "$stage" "$FAKE_REMOTE_ROOT"; then status=0; else status=$?; fi
else
  status=2
fi
rm -f -- "$remote_script"
exit "$status"
FAKE_SSH

cat > "$FAKE_BIN/scp" <<'FAKE_SCP'
#!/usr/bin/env bash
set -euo pipefail
while [[ "${1:-}" == -o ]]; do shift 2; done
source_file="$1"
destination="$2"
[[ "$destination" == "root@$FAKE_DEPLOY_HOST:"* ]] || exit 2
remote_path="${destination#*:}"
[[ "$remote_path" =~ ^/root/\.c1-host-setup\.stage\.[[:alnum:]]{6}/payload\.tar\.gz$ ]] \
  || exit 2
if [[ "${FAKE_SCP_FAIL:-0}" == 1 ]]; then exit 43; fi
target="$FAKE_REMOTE_ROOT/${remote_path#/root/}"
mkdir -p "${target%/*}"
cp "$source_file" "$target"
if [[ "${FAKE_SCP_TAMPER:-0}" == 1 ]]; then printf 'tampered\n' >> "$target"; fi
FAKE_SCP

cat > "$FAKE_BIN/sha256sum" <<'FAKE_SHA256SUM'
#!/usr/bin/env bash
set -euo pipefail
if [[ "${1:-}" == -c && "${2:-}" == - ]]; then
  read -r expected file
  actual="$(shasum -a 256 "$file" | awk '{print $1}')"
  [[ "$expected" == "$actual" ]] || { printf '%s: FAILED\n' "$file" >&2; exit 1; }
  printf '%s: OK\n' "$file"
else
  shasum -a 256 "$1"
fi
FAKE_SHA256SUM

cat > "$FAKE_BIN/rm" <<'FAKE_RM'
#!/usr/bin/env bash
set -euo pipefail
args=()
for arg in "$@"; do [[ "$arg" == -- ]] || args+=("$arg"); done
exec /bin/rm "${args[@]}"
FAKE_RM

cat > "$FAKE_BIN/ln" <<'FAKE_LN'
#!/usr/bin/env bash
set -euo pipefail
args=()
for arg in "$@"; do [[ "$arg" == -- ]] || args+=("$arg"); done
exec /bin/ln "${args[@]}"
FAKE_LN

cat > "$FAKE_BIN/readlink" <<'FAKE_READLINK'
#!/usr/bin/env bash
set -euo pipefail
[[ "${1:-}" != -- ]] || shift
if [[ "${FAKE_PROMOTION_ACTIVE:-0}" == 1 \
  && "${1:-}" == "$FAKE_REMOTE_ROOT/c1-host-setup" ]]; then
  [[ "$(<"$FAKE_LOCK_STATE_FILE")" == exclusive ]] \
    || { printf 'target inspected without promotion lock\n' >&2; exit 94; }
  printf 'target inspected under promotion lock\n' >> "$FAKE_TRACE"
fi
exec /usr/bin/readlink "$@"
FAKE_READLINK

cat > "$FAKE_BIN/stat" <<'FAKE_PROMOTE_STAT'
#!/usr/bin/env bash
set -euo pipefail
[[ "${1:-}" == -c && "${3:-}" == -- && -n "${4:-}" ]] || exit 2
format="$2"
path="$4"
python3 - "$format" "$path" <<'PROMOTE_STAT'
import os
import stat
import sys

fmt, path = sys.argv[1:]
item = os.lstat(path)
values = {
    "%a": format(stat.S_IMODE(item.st_mode), "o"),
    "%h": str(item.st_nlink),
    "%u:%g": f"{item.st_uid}:{item.st_gid}",
}
if fmt not in values:
    raise SystemExit(2)
print(values[fmt])
PROMOTE_STAT
FAKE_PROMOTE_STAT

cat > "$FAKE_BIN/mv" <<'FAKE_MV'
#!/usr/bin/env bash
set -euo pipefail
atomic_replace() {
  python3 - "$1" "$2" <<'ATOMIC_REPLACE'
import os
import sys
os.replace(sys.argv[1], sys.argv[2])
ATOMIC_REPLACE
}
if [[ "${1:-}" == -Tf ]]; then
  shift
  [[ "${1:-}" != -- ]] || shift
  source_path="$1"
  target_path="$2"
  [[ "${source_path%/*}" == "${target_path%/*}" ]] \
    || { printf 'promotion rename crossed directories\n' >&2; exit 93; }
  printf 'atomic mv -T target=%s existing-link=%s\n' "$target_path" \
    "$([[ -L "$target_path" ]] && printf yes || printf no)" >> "$FAKE_TRACE"
  if [[ "${FAKE_FAIL_PROMOTION:-0}" == after \
    && "$target_path" == "$FAKE_REMOTE_ROOT/c1-host-setup" \
    && ! -e "$FAKE_PROMOTION_FAILED_FILE" ]]; then
    atomic_replace "$source_path" "$target_path"
    : > "$FAKE_PROMOTION_FAILED_FILE"
    exit 71
  fi
  if [[ "${FAKE_FAIL_PROMOTION:-0}" == before \
    && "$target_path" == "$FAKE_REMOTE_ROOT/c1-host-setup" ]]; then exit 71; fi
  atomic_replace "$source_path" "$target_path"
  exit 0
fi
args=()
for arg in "$@"; do [[ "$arg" == -- ]] || args+=("$arg"); done
exec /bin/mv "${args[@]}"
FAKE_MV

cat > "$FAKE_BIN/chown" <<'FAKE_CHOWN'
#!/usr/bin/env bash
set -euo pipefail
printf 'chown %s\n' "$*" >> "$FAKE_TRACE"
FAKE_CHOWN

cat > "$FAKE_BIN/chmod" <<'FAKE_CHMOD'
#!/usr/bin/env bash
set -euo pipefail
printf 'chmod %s\n' "$*" >> "$FAKE_TRACE"
args=()
for arg in "$@"; do [[ "$arg" == -- ]] || args+=("$arg"); done
exec /bin/chmod "${args[@]}"
FAKE_CHMOD

cat > "$FAKE_BIN/tar" <<'FAKE_TAR'
#!/usr/bin/env bash
set -euo pipefail
args=()
for arg in "$@"; do
  case "$arg" in
    --no-same-owner|--no-same-permissions) ;;
    *) args+=("$arg");;
  esac
done
exec "$C1_TEST_REAL_TAR" "${args[@]}"
FAKE_TAR

cat > "$FAKE_BIN/find" <<'FAKE_FIND'
#!/usr/bin/env bash
set -euo pipefail
args=()
for arg in "$@"; do
  [[ "$arg" != /111 ]] || arg=+111
  args+=("$arg")
done
exec "$C1_TEST_REAL_FIND" "${args[@]}"
FAKE_FIND

chmod +x "$FAKE_BIN/ssh-keygen" "$FAKE_BIN/ssh" "$FAKE_BIN/scp" \
  "$FAKE_BIN/sha256sum" "$FAKE_BIN/rm" "$FAKE_BIN/ln" \
  "$FAKE_BIN/readlink" "$FAKE_BIN/stat" "$FAKE_BIN/mv" \
  "$FAKE_BIN/chown" "$FAKE_BIN/chmod" "$FAKE_BIN/tar" "$FAKE_BIN/find"
export FAKE_DEPLOY_HOST FAKE_KNOWN_HOSTS

run_fake_deploy() {
    HOST="$FAKE_DEPLOY_HOST" KNOWN_HOSTS="$FAKE_KNOWN_HOSTS" \
    PROMOTION_FLOCK_BIN="$FAKE_BIN/flock" \
    PROMOTION_LOCK_OWNER="$(id -u):$(id -g)" \
    FAKE_PROMOTION_ACTIVE=1 \
    FAKE_PROMOTION_FAILED_FILE="$FAKE_REMOTE_ROOT/.promotion-failed" \
    bash "$C1_DIR/deploy-bastion.sh"
}

promotion_waits_for_active_provisioner_test() (
  local fence_root="$FAKE_TMP/release-fence" owner payload_tree stage digest status
  owner="$(id -u):$(id -g)"
  rm -rf -- "$fence_root"
  mkdir -m 0700 -- "$fence_root"
  PROMOTION_LOCK_PATH="$fence_root/.c1-host-setup.promote.lock"
  PROMOTION_LOCK_OWNER="$owner"
  PROMOTION_FLOCK_BIN="$FAKE_BIN/flock"
  FAKE_STAT_OVERRIDE_PATH="$PROMOTION_LOCK_PATH"
  FAKE_STAT_OWNER_OVERRIDE="$owner"
  FAKE_STAT_MODE_OVERRIDE=600
  : > "$FAKE_LOCK_STATE_FILE"
  MAINTENANCE_TIMEOUT=10
  MAINTENANCE_DEADLINE=$((SECONDS + 10))
  # shellcheck disable=SC2329 # acquire_promotion_shared_lock calls these validators.
  assert_safe_managed_directory() {
    [[ -d "$1" && ! -L "$1" ]]
  }
  # shellcheck disable=SC2329 # acquire_promotion_shared_lock calls this validator.
  assert_safe_managed_file() {
    [[ -f "$1" && ! -L "$1" ]]
  }
  acquire_promotion_shared_lock

  payload_tree="$FAKE_TMP/release-fence-payload"
  mkdir -p -- "$payload_tree/c1-host-setup"
  printf '#!/usr/bin/env bash\nexit 0\n' > "$payload_tree/c1-host-setup/provision-bastion.sh"
  stage="$fence_root/.c1-host-setup.stage.ABC123"
  mkdir -m 0700 -- "$stage"
  tar -czf "$stage/payload.tar.gz" -C "$payload_tree" c1-host-setup
  digest="$(sha256sum "$stage/payload.tar.gz" | awk '{print $1}')"
  if PROMOTION_LOCK_TIMEOUT_SECONDS=1 PROMOTION_LOCK_OWNER="$owner" \
    PROMOTION_FLOCK_BIN="$FAKE_BIN/flock" \
    bash "$C1_DIR/promote-staged-tree.sh" "$digest" "$stage" "$fence_root" \
    > "$FAKE_TMP/promotion-fenced.out" 2>&1; then
    status=0
  else
    status=$?
  fi
  [[ "$status" == 2 ]] || {
    cat "$FAKE_TMP/promotion-fenced.out" >&2
    printf 'promotion unexpectedly returned status %s while shared lock was held\n' "$status" >&2
    exit 1
  }
  grep -Fq 'timed out acquiring promotion lock' "$FAKE_TMP/promotion-fenced.out" || {
    cat "$FAKE_TMP/promotion-fenced.out" >&2
    printf 'promotion did not report shared-lock contention\n' >&2
    exit 1
  }
  [[ ! -e "$fence_root/c1-host-setup" && ! -L "$fence_root/c1-host-setup" ]] || {
    printf 'promotion exposed a target before acquiring its exclusive lock\n' >&2
    exit 1
  }

  release_promotion_shared_lock
  : > "$FAKE_TRACE"
  stage="$fence_root/.c1-host-setup.stage.DEF456"
  mkdir -m 0700 -- "$stage"
  tar -czf "$stage/payload.tar.gz" -C "$payload_tree" c1-host-setup
  digest="$(sha256sum "$stage/payload.tar.gz" | awk '{print $1}')"
  if PROMOTION_LOCK_TIMEOUT_SECONDS=1 PROMOTION_LOCK_OWNER="$owner" \
    PROMOTION_FLOCK_BIN="$FAKE_BIN/flock" \
    bash "$C1_DIR/promote-staged-tree.sh" "$digest" "$stage" "$fence_root" \
    > "$FAKE_TMP/promotion-after-unlock.out" 2>&1; then
    status=0
  else
    status=$?
    cat "$FAKE_TMP/promotion-after-unlock.out" >&2
    printf 'promotion failed after shared lock was released with status %s\n' "$status" >&2
    exit 1
  fi
  [[ -L "$fence_root/c1-host-setup" ]]
  release_path="$(/usr/bin/readlink "$fence_root/c1-host-setup")"
  [[ -f "$release_path/provision-bastion.sh" ]]
  [[ "$(stat -c '%a' -- "$release_path")" == 755 ]]
  [[ "$(stat -c '%a' -- "$release_path/provision-bastion.sh")" == 644 ]]
  grep -Fq 'chown -R 0:0 -- ' "$FAKE_TRACE"
  grep -Fq 'chmod 0755 -- ' "$FAKE_TRACE"
  grep -Fq 'chmod 0644 -- ' "$FAKE_TRACE"
)
assert_status 'exclusive promotion waits until the provisioner releases its shared lock' 0 \
  promotion_waits_for_active_provisioner_test

transfer_failure_cleans_remote_stage_test() (
  FAKE_REMOTE_ROOT="$FAKE_TMP/remote-transfer-failure"
  mkdir -p -- "$FAKE_REMOTE_ROOT"
  export FAKE_REMOTE_ROOT FAKE_SCP_FAIL=1 FAKE_SCP_TAMPER=0 FAKE_FAIL_PROMOTION=0
  if run_fake_deploy > "$FAKE_TMP/transfer-failure.out" 2>&1; then status=0; else status=$?; fi
  [[ "$status" == 43 ]]
  [[ -z "$(find "$FAKE_REMOTE_ROOT" -maxdepth 1 -name '.c1-host-setup.stage.*' -print -quit)" ]]
)
assert_status 'failed archive transfer cleans the validated remote stage' 0 \
  transfer_failure_cleans_remote_stage_test

checksum_failure_cleans_remote_stage_test() (
  FAKE_REMOTE_ROOT="$FAKE_TMP/remote-checksum-failure"
  mkdir -p -- "$FAKE_REMOTE_ROOT"
  export FAKE_REMOTE_ROOT FAKE_SCP_FAIL=0 FAKE_SCP_TAMPER=1 FAKE_FAIL_PROMOTION=0
  if run_fake_deploy > "$FAKE_TMP/checksum-failure.out" 2>&1; then status=0; else status=$?; fi
  [[ "$status" != 0 ]]
  [[ -z "$(find "$FAKE_REMOTE_ROOT" -maxdepth 1 -name '.c1-host-setup.stage.*' -print -quit)" ]]
  [[ -z "$(find "$FAKE_REMOTE_ROOT" -maxdepth 1 -name '.c1-host-setup.release.*' -print -quit)" ]]
  [[ ! -e "$FAKE_REMOTE_ROOT/c1-host-setup" && ! -L "$FAKE_REMOTE_ROOT/c1-host-setup" ]]
)
assert_status 'checksum failure cleans staging without promoting a tree' 0 \
  checksum_failure_cleans_remote_stage_test

physical_target_is_rejected_untouched_test() (
  FAKE_REMOTE_ROOT="$FAKE_TMP/remote-physical-target"
  mkdir -p -- "$FAKE_REMOTE_ROOT/c1-host-setup"
  printf 'previous tree\n' > "$FAKE_REMOTE_ROOT/c1-host-setup/previous.txt"
  export FAKE_REMOTE_ROOT FAKE_SCP_FAIL=0 FAKE_SCP_TAMPER=0 FAKE_FAIL_PROMOTION=0
  if run_fake_deploy > "$FAKE_TMP/physical-target.out" 2>&1; then status=0; else status=$?; fi
  [[ "$status" == 2 ]]
  [[ -d "$FAKE_REMOTE_ROOT/c1-host-setup" && ! -L "$FAKE_REMOTE_ROOT/c1-host-setup" ]]
  [[ "$(<"$FAKE_REMOTE_ROOT/c1-host-setup/previous.txt")" == 'previous tree' ]]
  [[ -z "$(find "$FAKE_REMOTE_ROOT" -maxdepth 1 -name '.c1-host-setup.stage.*' -print -quit)" ]]
  [[ -z "$(find "$FAKE_REMOTE_ROOT" -maxdepth 1 -name '.c1-host-setup.release.*' -print -quit)" ]]
)
assert_status 'physical legacy target is rejected without moving or exposing a gap' 0 \
  physical_target_is_rejected_untouched_test

promotion_failure_restores_symlink_target_test() (
  FAKE_REMOTE_ROOT="$FAKE_TMP/remote-promotion-failure"
  mkdir -p -- "$FAKE_REMOTE_ROOT/.old-release"
  printf 'previous release\n' > "$FAKE_REMOTE_ROOT/.old-release/previous.txt"
  ln -s "$FAKE_REMOTE_ROOT/.old-release" "$FAKE_REMOTE_ROOT/c1-host-setup"
  export FAKE_REMOTE_ROOT FAKE_SCP_FAIL=0 FAKE_SCP_TAMPER=0 FAKE_FAIL_PROMOTION=after
  if run_fake_deploy > "$FAKE_TMP/promotion-failure.out" 2>&1; then status=0; else status=$?; fi
  [[ "$status" == 71 ]]
  [[ -L "$FAKE_REMOTE_ROOT/c1-host-setup" ]]
  [[ "$(readlink "$FAKE_REMOTE_ROOT/c1-host-setup")" == "$FAKE_REMOTE_ROOT/.old-release" ]]
  [[ "$(<"$FAKE_REMOTE_ROOT/c1-host-setup/previous.txt")" == 'previous release' ]]
  [[ -z "$(find "$FAKE_REMOTE_ROOT" -maxdepth 1 -name '.c1-host-setup.stage.*' -print -quit)" ]]
  [[ -z "$(find "$FAKE_REMOTE_ROOT" -maxdepth 1 -name '.c1-host-setup.release.*' -print -quit)" ]]
)
assert_status 'failed atomic symlink promotion restores the prior release' 0 \
  promotion_failure_restores_symlink_target_test

promotion_lock_serializes_before_target_read_test() (
  FAKE_REMOTE_ROOT="$FAKE_TMP/remote-promotion-lock"
  mkdir -p -- "$FAKE_REMOTE_ROOT/.old-release"
  printf 'old target\n' > "$FAKE_REMOTE_ROOT/.old-release/previous.txt"
  ln -s "$FAKE_REMOTE_ROOT/.old-release" "$FAKE_REMOTE_ROOT/c1-host-setup"
  export FAKE_REMOTE_ROOT FAKE_SCP_FAIL=0 FAKE_SCP_TAMPER=0 FAKE_FAIL_PROMOTION=0
  printf '2\n' > "$FAKE_FLOCK_BUSY_FILE"
  : > "$FAKE_TRACE"
  run_fake_deploy >/dev/null 2>&1
  [[ "$(<"$FAKE_FLOCK_BUSY_FILE")" == 0 ]]
  local contended lock_line mv_line unlock_line
  contended="$(grep -c '^flock contended ' "$FAKE_TRACE")"
  lock_line="$(grep -n '^flock exclusive ' "$FAKE_TRACE" | head -n1 | cut -d: -f1)"
  mv_line="$(grep -n '^atomic mv -T target=' "$FAKE_TRACE" | head -n1 | cut -d: -f1)"
  unlock_line="$(grep -n '^flock unlock ' "$FAKE_TRACE" | tail -n1 | cut -d: -f1)"
  [[ "$contended" == 2 ]]
  [[ -n "$lock_line" && -n "$mv_line" && -n "$unlock_line" ]]
  (( lock_line < mv_line && mv_line < unlock_line ))
  [[ -L "$FAKE_REMOTE_ROOT/c1-host-setup" ]]
  grep -Fq 'target inspected under promotion lock' "$FAKE_TRACE"
)
assert_status 'promotion lock contention is bounded and serialized before atomic switch' 0 \
  promotion_lock_serializes_before_target_read_test

repeat_copy_replaces_exact_active_tree_test() (
  FAKE_REMOTE_ROOT="$FAKE_TMP/remote-repeat-copy"
  rm -rf -- "$FAKE_REMOTE_ROOT"
  mkdir -p -- "$FAKE_REMOTE_ROOT"
  export FAKE_REMOTE_ROOT FAKE_SCP_FAIL=0 FAKE_SCP_TAMPER=0 FAKE_FAIL_PROMOTION=0
  run_fake_deploy >/dev/null 2>&1
  [[ -L "$FAKE_REMOTE_ROOT/c1-host-setup" ]]
  [[ -f "$FAKE_REMOTE_ROOT/c1-host-setup/provision-bastion.sh" \
    && ! -L "$FAKE_REMOTE_ROOT/c1-host-setup/provision-bastion.sh" ]]
  [[ -x "$FAKE_REMOTE_ROOT/c1-host-setup/apt-plan-guard.sh" ]]
  printf 'stale remote file\n' > "$FAKE_REMOTE_ROOT/c1-host-setup/remote-only.txt"
  run_fake_deploy >/dev/null 2>&1
  [[ -L "$FAKE_REMOTE_ROOT/c1-host-setup" ]]
  [[ ! -e "$FAKE_REMOTE_ROOT/c1-host-setup/remote-only.txt" ]]
  [[ ! -e "$FAKE_REMOTE_ROOT/c1-host-setup/c1-host-setup" ]]
  [[ -z "$(find "$FAKE_REMOTE_ROOT" -maxdepth 1 -name '.c1-host-setup.stage.*' -print -quit)" ]]
  grep -Fq 'target inspected under promotion lock' "$FAKE_TRACE"
  grep -Fq "atomic mv -T target=$FAKE_REMOTE_ROOT/c1-host-setup existing-link=yes" "$FAKE_TRACE"
)
assert_status 'repeat upload promotes an exact tree without nesting or stale files' 0 \
  repeat_copy_replaces_exact_active_tree_test

direct_invocation_requires_entry_lock_test() {
  local output status
  if output="$(env -u VELNOR_C1_PROMOTION_LOCK_FD \
    bash "$C1_DIR/provision-bastion.sh" --help 2>&1)"; then
    status=0
  else
    status=$?
  fi
  [[ "$status" == 2 ]] \
    && [[ "$output" == *'shared promotion-lock entry wrapper'* ]]
}
assert_status 'direct provisioner invocation fails before reading release files' 0 \
  direct_invocation_requires_entry_lock_test

entry_wrapper_inherits_and_verifies_shared_lock_test() (
  local entry_lock="$FAKE_TMP/entry-wrapper.promote.lock"
  : > "$entry_lock"
  exec 9<> "$entry_lock"
  /usr/bin/flock --shared --nonblock 9
  export VELNOR_C1_PROMOTION_LOCK_FD=9
  export C1_TEST_PROVISION_PATH="$C1_DIR/provision-bastion.sh"
  export C1_TEST_PROMOTION_LOCK="$entry_lock"
  /bin/bash -c '
    set -euo pipefail
    # shellcheck disable=SC1090 # test passes a concrete source path.
    . "$C1_TEST_PROVISION_PATH"
    [[ "$(/usr/bin/readlink -- "/proc/$$/fd/9")" == "$C1_TEST_PROMOTION_LOCK" ]]
    require_shared_promotion_entry_lock \
      "$VELNOR_C1_PROMOTION_LOCK_FD" "$C1_TEST_PROMOTION_LOCK"
    if /usr/bin/flock --exclusive --nonblock "$C1_TEST_PROMOTION_LOCK" true; then
      printf "exclusive lock unexpectedly passed the wrapper shared lock\\n" >&2
      exit 1
    fi
  '
)
wrapper_missing_prerequisites=()
[[ -x /usr/bin/flock ]] || wrapper_missing_prerequisites+=("/usr/bin/flock")
[[ -d "/proc/$$/fd" ]] || wrapper_missing_prerequisites+=("/proc/<pid>/fd")
if ((${#wrapper_missing_prerequisites[@]} == 0)); then
  assert_status 'entry wrapper passes a live shared lock FD through Bash' 0 \
    entry_wrapper_inherits_and_verifies_shared_lock_test
elif [[ "$(uname -s)" == Linux ]]; then
  printf 'FAIL inherited-FD integration missing Linux prerequisite(s): %s\n' \
    "${wrapper_missing_prerequisites[*]}" >&2
  exit 1
else
  printf 'SKIP entry wrapper inherited-FD integration; missing prerequisite(s): %s\n' \
    "${wrapper_missing_prerequisites[*]}"
fi

entry_guard_runs_before_release_files_test() {
  local guard_line c1_dir_line
  guard_line="$(grep -nF 'if [[ "${BASH_SOURCE[0]}" == "$0" ]]' \
    "$C1_DIR/provision-bastion.sh" | head -n 1 | cut -d: -f1)"
  c1_dir_line="$(grep -n '^C1_DIR=' "$C1_DIR/provision-bastion.sh" | head -n 1 | cut -d: -f1)"
  [[ "$guard_line" =~ ^[0-9]+$ && "$c1_dir_line" =~ ^[0-9]+$ ]] \
    && (( guard_line < c1_dir_line ))
}
assert_status 'entry lock guard precedes pin and release-file reads' 0 \
  entry_guard_runs_before_release_files_test

readme_deploy_block_is_fail_fast_test() {
  local first_line
  first_line="$(awk '/^```bash$/ { inside=1; next } inside { if ($0 == "```") exit; print; exit }' "$C1_DIR/README.md")"
  [[ "$first_line" == 'set -euo pipefail' ]] \
    || { printf 'README deployment block does not enable fail-fast mode\n' >&2; return 1; }
  grep -Fq '"$C1/deploy-bastion.sh"' "$C1_DIR/README.md"
  [[ "$(grep -Fc 'exec 9<>/root/.c1-host-setup.promote.lock' "$C1_DIR/README.md" || true)" == 3 ]]
  [[ "$(grep -Fc '/usr/bin/flock --shared --nonblock 9' "$C1_DIR/README.md" || true)" == 3 ]]
  [[ "$(grep -Fc 'export VELNOR_C1_PROMOTION_LOCK_FD=9' "$C1_DIR/README.md" || true)" == 3 ]]
  [[ "$(grep -Fc '/bin/bash /root/c1-host-setup/provision-bastion.sh' "$C1_DIR/README.md" || true)" == 3 ]]
}
assert_status 'README cannot continue to check/apply after a failed staged upload' 0 \
  readme_deploy_block_is_fail_fast_test

unset -f stat
