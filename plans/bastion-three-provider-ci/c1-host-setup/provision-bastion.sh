#!/usr/bin/env bash
#
# C1 bastion host-setup — idempotent, non-destructive provisioner.
#
# Derived from ChainArgos/java-monorepo ansible-configs §6.1 paths at audited
# live head 218a44b28984acf4ceee24cd1d9d6ccb8c38ae37 (verified no-drift
# 2026-09-18). That source is the reference; this script is the bastion
# adaptation with the §6/§7 gaps closed (pins, key fingerprints, drain+holds).
#
# Usage (run ON the target as root; see README.md for the C-gate command):
#   provision-bastion.sh [--check]     # read-only; exit 1 pending, 2 if state/resolution unknown
#   provision-bastion.sh --help
#
# Environment:
#   VELNOR_C1_DOCKER_POOLS=1   opt-in 172.30.0.0/16 daemon pools (default: off,
#                              selene-style omit; refused if routes conflict)
#   VELNOR_C1_ALLOW_RESTART=1  allow explicitly accepted non-Velnor containers
#                              after Velnor admission is closed (default: refuse)
#   VELNOR_C1_DRAIN_TIMEOUT_SECONDS=10800  wait bound for Velnor jobs and lock
#   Package/repository pins may be overridden for reviewed re-resolves;
#   authenticated Docker key fingerprints are fixed readonly constants.
#
# NEVER in this script (structural, grep-verifiable):
#   no `apt-get upgrade/dist-upgrade`, no `autoremove`, no `docker system prune`,
#   no release upgrade, no mirror rewrite, no block-device/storage commands,
#   no direct firewall-policy or SSH changes (Docker may add its own rules when
#   started), no libvirt/KVM/QEMU, no per-repo reservations.
# Explicitly excluded from scope (see README.md): the runbook Drain-Docker
# block, drive-init playbooks, mise/nushell/holla repos, mise toolchains.
#
set -euo pipefail

ENTRY_PROMOTION_LOCK_PATH=/root/.c1-host-setup.promote.lock

require_shared_promotion_entry_lock() {
  local entry_lock_fd="${1:-}"
  local lock_path="${2:-}"
  local opened_path=
  [[ "$entry_lock_fd" =~ ^[0-9]+$ && -n "$lock_path" ]] || return 1
  [[ -e "/proc/$$/fd/$entry_lock_fd" ]] || return 1
  opened_path="$(/usr/bin/readlink -- "/proc/$$/fd/$entry_lock_fd")" || return 1
  [[ "$opened_path" == "$lock_path" ]] || return 1
  /usr/bin/flock --shared --nonblock "$entry_lock_fd"
}

if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
  entry_lock_fd="${VELNOR_C1_PROMOTION_LOCK_FD:-}"
  if ! require_shared_promotion_entry_lock \
    "$entry_lock_fd" "$ENTRY_PROMOTION_LOCK_PATH"; then
    printf 'run the provisioner through the shared promotion-lock entry wrapper before reading release files\n' >&2
    exit 2
  fi
fi

C1_DIR="$(cd -P -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
# shellcheck disable=SC1091 # resolved relative to C1_DIR at runtime
. "$C1_DIR/pins.env"
# shellcheck disable=SC1091
. "$C1_DIR/targets.env"

CHECK=0
PENDING=0
ALLOW_RESTART="${VELNOR_C1_ALLOW_RESTART:-0}"
WANT_POOLS="${VELNOR_C1_DOCKER_POOLS:-0}"
PACKAGE_LOCK_PATH=/run/velnor/package-transaction.lock
FLOCK_BIN=/usr/bin/flock
PROMOTION_LOCK_PATH="$ENTRY_PROMOTION_LOCK_PATH"
PROMOTION_FLOCK_BIN=/usr/bin/flock
PROMOTION_LOCK_OWNER=0:0
TIMEOUT_BIN=/usr/bin/timeout
PACKAGE_LOCK_FD=
PACKAGE_LOCK_HELD=0
RUNNER_INSTALLED=0
PERMIT_LEDGER_ROSTER_ROOT=/
SYSTEMD_VENDOR_UNIT_DIRS=(/lib/systemd/system /usr/lib/systemd/system)
STOCK_DAEMON_UNIT_SHA256=2eea43ab6e6dc4c05959d5b06e61352cdc556cd6fbbe519fd71a9d4a29811c2b
STOCK_DAEMON_TEMPLATE_UNIT_SHA256=8a2f7eaa6b09486b8882034a632b9183b14ce13bb8cd507ece27b26e1cb50159
PROMOTION_LOCK_FD=
PROMOTION_LOCK_HELD=0
MAINTENANCE_READY=0
MAINTENANCE_TIMEOUT="${VELNOR_C1_DRAIN_TIMEOUT_SECONDS:-10800}"
MAINTENANCE_DEADLINE=0
ADMISSION_UNITS=()
ADMISSION_CAPTURED=0
ADMISSION_RESTORE_ATTEMPTED=0
CLEANUP_PATHS=()
CLEANUP_DIRECTORIES=()
SAFE_APT_CONFIG=
SAFE_APT_DIRECTORY=
APT_SAFE_TEMP_ROOT=/tmp
DOCKER_RUNTIME_UNKNOWN=0
DOCKER_RUNTIME_SNAPSHOT=
DOCKER_CHANGE_STARTED=0
DOCKER_LOCAL_HEALTH_VERIFIED=0
VELNOR_LOCK_MISSING=0
DOCKER_LOCAL_HOST=unix:///run/docker.sock
DOCKER_CGROUP_MARKER_PATH=/sys/fs/cgroup/cgroup.controllers
DOCKER_DAEMON_CONFIG_PATH=/etc/docker/daemon.json
DOCKER_APT_SOURCE_DIR=/etc/apt/sources.list.d
DOCKER_APT_SOURCE_FILE=/etc/apt/sources.list.d/docker.list
DOCKER_APT_LEGACY_SOURCE_FILE=/etc/apt/sources.list.d/docker-ce.list
DOCKER_APT_MAIN_SOURCE_FILE=/etc/apt/sources.list
DOCKER_APT_KEY_FILE=/etc/apt/keyrings/docker.asc
DEBIAN_APT_KEYRINGS=(
  /usr/share/keyrings/debian-archive-keyring.gpg
  /usr/share/keyrings/debian-archive-keyring.pgp
)
DOCKER_SOCKET_PATHS=(/run/docker.sock /var/run/docker.sock)
APPROVED_APT_PLAN_FILE=

log()  { printf '==> %s\n' "$*"; }
warn() { printf '!!! %s\n' "$*" >&2; }
die()  { printf '### FATAL: %s\n' "$1" >&2; exit "${2:-1}"; }

# mut: the single mutation choke point. In --check mode it records the plan
# and runs nothing; in apply mode it logs and executes.
mut() {
  if [[ "$CHECK" -eq 1 ]]; then
    PENDING=$((PENDING + 1))
    printf '    would: %s\n' "$*"
    return 0
  fi
  log "run: $*"
  maintenance_command "$@"
}

maintenance_deadline_value() {
  if [[ "${MAINTENANCE_DEADLINE:-0}" =~ ^[1-9][0-9]*$ ]]; then
    printf '%s\n' "$MAINTENANCE_DEADLINE"
  else
    printf '%s\n' "$((SECONDS + MAINTENANCE_TIMEOUT))"
  fi
}

maintenance_seconds_remaining() {
  local deadline remaining
  deadline="$(maintenance_deadline_value)"
  remaining=$((deadline - SECONDS))
  (( remaining > 0 )) || return 1
  printf '%s\n' "$remaining"
}

maintenance_command() {
  local remaining command_name
  if [[ "$#" -gt 0 ]]; then
    command_name="${1##*/}"
    if declare -F "$command_name" >/dev/null 2>&1; then
      "$@"
      return
    fi
  fi
  remaining="$(maintenance_seconds_remaining)" || {
    warn "maintenance deadline expired before command: $*"
    return 124
  }
  [[ -x "$TIMEOUT_BIN" ]] || {
    warn "timeout utility is missing; cannot bound maintenance command: $*"
    return 127
  }
  "$TIMEOUT_BIN" --foreground --signal=TERM --kill-after=5s "${remaining}s" "$@"
}

usage() {
  sed -n '2,/^set -euo/p' "${BASH_SOURCE[0]}" | sed 's/^# \?//'
}

cleanup() {
  local status=$? path
  trap - EXIT
  set +e
  for path in "${CLEANUP_PATHS[@]}"; do rm -f -- "$path"; done
  for path in "${CLEANUP_DIRECTORIES[@]}"; do
    rmdir -- "$path" 2>/dev/null || warn "could not remove private temporary directory $path"
  done
  if [[ "$status" -ne 0 && "$CHECK" -eq 0 ]]; then
    if [[ "$DOCKER_CHANGE_STARTED" == 1 && "$DOCKER_LOCAL_HEALTH_VERIFIED" != 1 ]]; then
      warn "failed setup keeps every Velnor unit stopped because local Docker health was not verified"
      stop_all_active_velnor_units_failsafe
    elif [[ "$ADMISSION_RESTORE_ATTEMPTED" == 1 ]]; then
      stop_all_active_velnor_units_failsafe
      warn "failed setup keeps Velnor units stopped after an unsuccessful restore: ${ADMISSION_UNITS[*]}"
    elif [[ "$PACKAGE_LOCK_HELD" == 1 ]]; then
      # With the exclusive lock held, any newly activating job is waiting for
      # its shared ExecStart lock. Cancel it before releasing that lock.
      stop_all_active_velnor_units_failsafe
      if [[ "${#ADMISSION_UNITS[@]}" -gt 0 ]]; then
        ADMISSION_RESTORE_ATTEMPTED=1
        release_package_lock
        if systemd_available && start_admission_units_after_unlock; then
          log "restored Velnor admission units after failed setup: ${ADMISSION_UNITS[*]}"
          ADMISSION_UNITS=()
        else
          stop_all_active_velnor_units_failsafe
          warn "failed setup keeps previously active Velnor units stopped: ${ADMISSION_UNITS[*]}"
        fi
      fi
    elif [[ "${#ADMISSION_UNITS[@]}" -gt 0 ]]; then
      ADMISSION_RESTORE_ATTEMPTED=1
      release_package_lock
      if systemd_available && start_admission_units_after_unlock; then
        log "restored Velnor admission units after failed setup: ${ADMISSION_UNITS[*]}"
        ADMISSION_UNITS=()
      else
        stop_all_active_velnor_units_failsafe
        warn "failed setup keeps previously active Velnor units stopped: ${ADMISSION_UNITS[*]}"
      fi
    fi
  fi
  release_package_lock
  release_promotion_shared_lock
  exit "$status"
}

acquire_promotion_shared_lock() {
  local parent metadata owner mode links deadline status
  [[ "$PROMOTION_LOCK_HELD" == 0 ]] || return 0
  parent="${PROMOTION_LOCK_PATH%/*}"
  assert_safe_managed_directory "$parent" "C1 promotion-lock directory" \
    || die "C1 promotion-lock directory is unsafe" 2
  [[ ! -L "$PROMOTION_LOCK_PATH" ]] \
    || die "C1 promotion lock is a symlink" 2
  if [[ ! -e "$PROMOTION_LOCK_PATH" ]]; then
    (umask 077; set -o noclobber; : > "$PROMOTION_LOCK_PATH") 2>/dev/null || true
  fi
  assert_safe_managed_file "$PROMOTION_LOCK_PATH" "C1 promotion lock" \
    || die "C1 promotion lock is unsafe" 2
  metadata="$(stat -c '%u:%g %a %h' -- "$PROMOTION_LOCK_PATH")" \
    || die "cannot inspect C1 promotion lock" 2
  read -r owner mode links <<< "$metadata"
  [[ "$owner" == "$PROMOTION_LOCK_OWNER" && "$mode" == 600 && "$links" == 1 ]] \
    || die "C1 promotion lock must be root-owned, single-link, mode 0600" 2
  exec {PROMOTION_LOCK_FD}<>"$PROMOTION_LOCK_PATH" \
    || die "cannot open C1 promotion lock" 2
  deadline="$(maintenance_deadline_value)"
  while :; do
    if "$PROMOTION_FLOCK_BIN" --shared --nonblock "$PROMOTION_LOCK_FD"; then
      PROMOTION_LOCK_HELD=1
      return 0
    else
      status=$?
    fi
    [[ "$status" == 1 ]] || die "cannot acquire C1 shared promotion lock" 2
    (( SECONDS < deadline )) || die "timed out acquiring C1 shared promotion lock" 2
    sleep 1 || die "interrupted while waiting for C1 shared promotion lock" 2
  done
}

release_promotion_shared_lock() {
  if [[ "$PROMOTION_LOCK_HELD" == 1 && -n "$PROMOTION_LOCK_FD" ]]; then
    "$PROMOTION_FLOCK_BIN" --unlock "$PROMOTION_LOCK_FD" \
      || warn "could not explicitly unlock C1 promotion lock"
    exec {PROMOTION_LOCK_FD}>&-
    PROMOTION_LOCK_FD=
    PROMOTION_LOCK_HELD=0
  fi
}

track_temp() {
  CLEANUP_PATHS+=("$1")
}

ensure_safe_apt_config() {
  [[ -n "$SAFE_APT_CONFIG" && -f "$SAFE_APT_CONFIG" && ! -L "$SAFE_APT_CONFIG" ]] \
    && { export APT_CONFIG="$SAFE_APT_CONFIG"; return 0; }

  SAFE_APT_DIRECTORY="$(mktemp -d "$APT_SAFE_TEMP_ROOT/velnor-c1-apt.XXXXXX")" \
    || die "cannot create private APT configuration directory" 2
  chmod 0700 "$SAFE_APT_DIRECTORY" \
    || die "cannot secure private APT configuration directory" 2
  mkdir -m 0700 "$SAFE_APT_DIRECTORY/parts" \
    || die "cannot create private empty APT configuration parts directory" 2
  SAFE_APT_CONFIG="$SAFE_APT_DIRECTORY/apt.conf"
  cat > "$SAFE_APT_CONFIG" <<APT_CONFIG_EOF
Dir::Etc::Parts "$SAFE_APT_DIRECTORY/parts";
Dir::Etc::main "/dev/null";
Dir::Etc::sourcelist "$DOCKER_APT_MAIN_SOURCE_FILE";
Dir::Etc::sourceparts "$DOCKER_APT_SOURCE_DIR";
Acquire::AllowInsecureRepositories "false";
Acquire::AllowDowngradeToInsecureRepositories "false";
Acquire::AllowWeakRepositories "false";
APT::Get::AllowUnauthenticated "false";
Debug::NoLocking "false";
APT_CONFIG_EOF
  chmod 0600 "$SAFE_APT_CONFIG" \
    || die "cannot secure private APT configuration file" 2
  chown 0:0 "$SAFE_APT_CONFIG" "$SAFE_APT_DIRECTORY" "$SAFE_APT_DIRECTORY/parts" \
    || die "cannot set private APT configuration ownership" 2
  assert_safe_managed_directory "$SAFE_APT_DIRECTORY" "private APT config directory" \
    || die "private APT configuration directory has unsafe metadata" 2
  assert_safe_managed_directory "$SAFE_APT_DIRECTORY/parts" "private APT config parts directory" \
    || die "private APT config parts directory has unsafe metadata" 2
  assert_safe_managed_file "$SAFE_APT_CONFIG" "private APT config file" \
    || die "private APT configuration file has unsafe metadata" 2
  track_temp "$SAFE_APT_CONFIG"
  CLEANUP_DIRECTORIES+=("$SAFE_APT_DIRECTORY/parts" "$SAFE_APT_DIRECTORY")
  export APT_CONFIG="$SAFE_APT_CONFIG"
}

apt_command() {
  ensure_safe_apt_config
  maintenance_command env LC_ALL=C "$@"
}

write_text_file() {
  printf '%s\n' "$1" > "$2"
}

# ---------------------------------------------------------------- pre-flight
# Read-only. Runs first in both modes; the route read precedes every pool
# decision and the running-work read precedes every drain gate.

POOL_CONFLICT=0
WORK_INVENTORY_UNKNOWN=0

cidr_to_network() { # IPv4 CIDR or address -> unsigned network integer + prefix
  local cidr="$1" address prefix octet value mask network
  local -a octets=()
  if [[ "$cidr" == */* ]]; then
    address="${cidr%/*}"
    prefix="${cidr#*/}"
  else
    address="$cidr"
    prefix=32
  fi
  [[ "$prefix" =~ ^(0|[1-9][0-9]?)$ ]] || return 2
  (( prefix <= 32 )) || return 2
  IFS=. read -r -a octets <<< "$address"
  (( ${#octets[@]} == 4 )) || return 2
  value=0
  for octet in "${octets[@]}"; do
    [[ "$octet" =~ ^[0-9]{1,3}$ ]] || return 2
    (( 10#$octet <= 255 )) || return 2
    value=$(( (value << 8) | (10#$octet) ))
  done
  if (( prefix == 0 )); then
    mask=0
  else
    mask=$(( (0xffffffff << (32 - prefix)) & 0xffffffff ))
  fi
  network=$(( value & mask ))
  printf '%s %s\n' "$network" "$prefix"
}

cidr_overlaps() { # 0 overlap, 1 disjoint, 2 invalid CIDR
  local left right left_network left_prefix right_network right_prefix common mask
  left="$(cidr_to_network "$1")" || return 2
  right="$(cidr_to_network "$2")" || return 2
  read -r left_network left_prefix <<< "$left"
  read -r right_network right_prefix <<< "$right"
  common="$left_prefix"
  (( right_prefix < common )) && common="$right_prefix"
  if (( common == 0 )); then
    mask=0
  else
    mask=$(( (0xffffffff << (32 - common)) & 0xffffffff ))
  fi
  (( (left_network & mask) == (right_network & mask) ))
}

route_cidrs_from_ip() {
  local line destination
  local -a fields=()
  while IFS= read -r line; do
    [[ -n "$line" ]] || continue
    read -r -a fields <<< "$line"
    destination="${fields[0]:-}"
    case "$destination" in
      local|broadcast|unicast|multicast|throw|unreachable|prohibit|blackhole|nat|anycast)
        destination="${fields[1]:-}"
        ;;
    esac
    [[ -n "$destination" ]] || return 2
    [[ "$destination" == default ]] && continue
    [[ "$destination" == *:* ]] && continue # IPv6 cannot overlap an IPv4 pool.
    if [[ "$destination" == *.* || "$destination" == */* ]]; then
      cidr_to_network "$destination" >/dev/null || return 2
      printf '%s\n' "$destination"
    else
      return 2
    fi
  done
}

route_cidrs_from_proc() {
  local interface destination mask destination_value mask_value prefix seen_zero bit
  while read -r interface destination _ _ _ _ _ mask _ _ _; do
    [[ "$interface" == Iface ]] && continue
    [[ -n "${interface:-}" ]] || continue
    [[ "$destination" =~ ^[[:xdigit:]]{8}$ && "$mask" =~ ^[[:xdigit:]]{8}$ ]] || return 2
    destination_value=$(( (16#${destination:6:2} << 24) | (16#${destination:4:2} << 16) | (16#${destination:2:2} << 8) | 16#${destination:0:2} ))
    mask_value=$(( (16#${mask:6:2} << 24) | (16#${mask:4:2} << 16) | (16#${mask:2:2} << 8) | 16#${mask:0:2} ))
    prefix=0
    seen_zero=0
    for ((bit = 31; bit >= 0; bit--)); do
      if (( mask_value & (1 << bit) )); then
        (( seen_zero == 0 )) || return 2
        prefix=$((prefix + 1))
      else
        seen_zero=1
      fi
    done
    printf '%d.%d.%d.%d/%d\n' \
      "$(((destination_value >> 24) & 255))" \
      "$(((destination_value >> 16) & 255))" \
      "$(((destination_value >> 8) & 255))" \
      "$((destination_value & 255))" "$prefix"
  done
}

preflight_os() {
  log "pre-flight: OS/arch"
  [[ -f /etc/os-release ]] || die "no /etc/os-release; refusing unknown OS"
  # shellcheck disable=SC1091
  . /etc/os-release
  [[ "${ID:-}" == "debian" ]] || die "expected Debian, found ID=${ID:-unknown}"
  [[ "${VERSION_CODENAME:-}" == "trixie" ]] || die "expected Debian 13 (trixie), found ${VERSION_CODENAME:-unknown}"
  local arch; arch="$(dpkg --print-architecture)"
  [[ "$arch" == "amd64" ]] || die "expected amd64, found $arch"
  log "OS: Debian ${VERSION_ID:-?} (${VERSION_CODENAME}) ${arch}"
}

preflight_routes() {
  log "pre-flight: host routes (pool-conflict read)"
  local routes="" route_cidrs="" route_cidr
  if command -v ip >/dev/null 2>&1; then
    routes="$(ip -4 route show table all 2>/dev/null)" \
      || { warn "ip route read failed; pools refused"; POOL_CONFLICT=1; [[ "$WANT_POOLS" != "1" ]] || die "requested Docker pools require a complete route inventory" 2; return 0; }
    route_cidrs="$(route_cidrs_from_ip <<< "$routes")" \
      || { warn "could not parse complete IPv4 route table; pools refused"; POOL_CONFLICT=1; [[ "$WANT_POOLS" != "1" ]] || die "requested Docker pools require a complete route inventory" 2; return 0; }
  elif [[ -f /proc/net/route ]]; then
    routes="$(cat /proc/net/route)"
    route_cidrs="$(route_cidrs_from_proc <<< "$routes")" \
      || { warn "could not parse /proc/net/route; pools refused"; POOL_CONFLICT=1; [[ "$WANT_POOLS" != "1" ]] || die "requested Docker pools require a complete route inventory" 2; return 0; }
  else
    warn "no 'ip' and no /proc/net/route: route read impossible, pools refused"
    POOL_CONFLICT=1
    [[ "$WANT_POOLS" != "1" ]] || die "requested Docker pools require a complete route inventory" 2
    return 0
  fi
  printf '%s\n' "$routes"
  while IFS= read -r route_cidr; do
    [[ -n "$route_cidr" ]] || continue
    if cidr_overlaps 172.30.0.0/16 "$route_cidr"; then
      warn "host route $route_cidr overlaps configured Docker pool 172.30.0.0/16"
      POOL_CONFLICT=1
      break
    else
      local overlap_status=$?
      if [[ "$overlap_status" -eq 2 ]]; then
        warn "invalid route CIDR $route_cidr; pools refused"
        POOL_CONFLICT=1
        break
      fi
    fi
  done <<< "$route_cidrs"
  if [[ "$POOL_CONFLICT" -eq 0 ]]; then
    log "no route CIDR overlaps 172.30.0.0/16"
  elif [[ "$WANT_POOLS" == "1" ]]; then
    die "requested Docker address pool overlaps a host route or route inventory was incomplete" 2
  fi
}

running_containers() {
  local ids line count=0
  ids="$(docker_local ps -q 2>/dev/null)" || return 2
  while IFS= read -r line; do
    [[ -n "$line" ]] || continue
    count=$((count + 1))
  done <<< "$ids"
  printf '%s\n' "$count"
}

docker_local() {
  command -v docker >/dev/null 2>&1 || return 2
  maintenance_command /usr/bin/env -u DOCKER_HOST -u DOCKER_CONTEXT -u DOCKER_CONFIG \
    docker --host "$DOCKER_LOCAL_HOST" "$@"
}

docker_info_snapshot() {
  local snapshot root storage logging driver cgroup
  snapshot="$(docker_local info --format '{{.DockerRootDir}}|{{.Driver}}|{{.LoggingDriver}}|{{.CgroupDriver}}|{{.CgroupVersion}}' 2>/dev/null)" \
    || return 2
  IFS='|' read -r root storage logging driver cgroup <<< "$snapshot"
  [[ -n "$root" && -n "$storage" && -n "$logging" && -n "$driver" && -n "$cgroup" ]] \
    || return 2
  printf '%s\n' "$snapshot"
}

require_docker_info_snapshot() {
  docker_info_snapshot \
    || die "Docker runtime information is unknown (missing Docker CLI or failed docker info)" 2
}

systemd_available() {
  [[ -d /run/systemd/system ]] && command -v systemctl >/dev/null 2>&1
}

preflight_systemd() {
  systemd_available \
    || die "C1 host setup requires a running systemd manager; refusing all host changes" 2
}

wait_for_units_stopped() { # bound stop completion despite long systemd TimeoutStopSec
  local reason="$1" deadline unit state pending
  shift
  deadline="$(maintenance_deadline_value)"
  while :; do
    pending=0
    for unit in "$@"; do
      state="$(maintenance_command systemctl show --property=ActiveState --value "$unit" 2>/dev/null)" \
        || die "cannot verify stop state for $unit while $reason" 2
      case "$state" in
        inactive|failed|dead) ;;
        active|activating|reloading|deactivating) pending=1 ;;
        *) die "unknown systemd state '$state' for $unit while $reason" 2 ;;
      esac
    done
    [[ "$pending" == 0 ]] && return 0
    (( SECONDS < deadline )) \
      || die "timed out waiting for units to stop while $reason: $*" 2
    sleep 1
  done
}

stop_all_active_velnor_units_failsafe() {
  local units unit state
  local -a active=()
  if ! systemd_available; then
    warn "cannot verify Velnor admission closed without systemd; retaining the maintenance barrier"
    while :; do sleep 30; done
  fi
  while :; do
    if ! units="$(active_velnor_units)"; then
      warn "Velnor unit inventory failed during fail-closed cleanup; retrying while retaining the lock"
      sleep 2
      continue
    fi
    [[ -n "$units" ]] || return 0
    active=()
    while IFS= read -r unit; do
      [[ -n "$unit" ]] && active+=("$unit")
    done <<< "$units"
    if [[ "${#active[@]}" -eq 0 ]]; then return 0; fi
    if declare -F systemctl >/dev/null 2>&1; then
      systemctl --no-block stop "${active[@]}" >/dev/null 2>&1 || true
    elif [[ -x "$TIMEOUT_BIN" ]]; then
      "$TIMEOUT_BIN" --foreground --signal=TERM --kill-after=5s 30s \
        systemctl --no-block stop "${active[@]}" >/dev/null 2>&1 || true
    else
      systemctl --no-block stop "${active[@]}" >/dev/null 2>&1 || true
    fi
    for unit in "${active[@]}"; do
      if declare -F systemctl >/dev/null 2>&1; then
        state="$(systemctl show --property=ActiveState --value "$unit" 2>/dev/null)" || state=unknown
      elif [[ -x "$TIMEOUT_BIN" ]]; then
        state="$("$TIMEOUT_BIN" --foreground --signal=TERM --kill-after=5s 30s \
          systemctl show --property=ActiveState --value "$unit" 2>/dev/null)" || state=unknown
      else
        state="$(systemctl show --property=ActiveState --value "$unit" 2>/dev/null)" || state=unknown
      fi
      case "$state" in inactive|failed|dead) ;; *) break ;; esac
    done
    sleep 1
  done
}

docker_service_inactive() {
  local load_state state unit_files loaded_units
  # A missing systemd manager cannot prove the Docker service is absent. The
  # caller may bootstrap only from an authoritative unit state or inventory.
  systemd_available || return 1

  if load_state="$(systemctl show --property=LoadState --value docker.service 2>/dev/null)"; then
    case "$load_state" in
      loaded)
        state="$(systemctl show --property=ActiveState --value docker.service 2>/dev/null)" \
          || return 1
        case "$state" in
          inactive|failed) return 0 ;;
          *) return 1 ;;
        esac
        ;;
      not-found) return 0 ;;
      *) return 1 ;;
    esac
  fi

  # Some systemd versions return failure for show on an absent unit. Treat it
  # as absent only when both complete inventories succeed and show no unit.
  unit_files="$(systemctl list-unit-files --type=service --no-legend --no-pager docker.service 2>/dev/null)" \
    || return 1
  loaded_units="$(systemctl list-units --all --plain --no-legend --no-pager docker.service 2>/dev/null)" \
    || return 1
  [[ -z "$unit_files" && -z "$loaded_units" ]]
}

docker_runtime_inactive() {
  local rc socket
  docker_service_inactive || return 1
  if command -v pgrep >/dev/null 2>&1; then
    if pgrep -x dockerd >/dev/null 2>&1; then return 1; else rc=$?; fi
    [[ "$rc" -eq 1 ]] || return 1
  else
    return 1
  fi
  for socket in "${DOCKER_SOCKET_PATHS[@]}"; do
    [[ ! -S "$socket" ]] || return 1
  done
}

container_count_for_maintenance() {
  local count
  if count="$(running_containers)"; then
    printf '%s\n' "$count"
    return 0
  fi
  if [[ "$CHECK" -eq 0 ]] && docker_runtime_inactive; then
    warn "Docker inventory unavailable while the daemon is confirmed stopped; treating running-container count as zero"
    printf '0\n'
    return 0
  fi
  return 2
}

active_velnor_units() {
  systemd_available || return 0
  local rows unit state
  rows="$(maintenance_command systemctl list-units --all --plain --no-legend 'velnor*' 2>/dev/null)" || return 2
  while read -r unit _ state _ _; do
    [[ "$state" == active || "$state" == activating || "$state" == reloading || "$state" == deactivating ]] || continue
    [[ "$unit" == velnor* ]] || continue
    printf '%s\n' "$unit"
  done <<< "$rows"
}

is_admission_unit() {
  case "$1" in
    velnor-daemon.service|velnor-daemon@*.service|velnor-controller@*.service|velnor-slot@*.service|velnor-guardian.service|velnor*.socket|velnor*.timer|velnor*.path)
      return 0
      ;;
    *) return 1 ;;
  esac
}

close_velnor_admission() {
  systemd_available || return 0
  local units unit remaining
  local -a newly_active=()
  units="$(active_velnor_units)" \
    || die "cannot inventory active Velnor services; refusing maintenance" 2
  while IFS= read -r unit; do
    is_admission_unit "$unit" || continue
    if [[ "$ADMISSION_CAPTURED" == 0 ]]; then
      ADMISSION_UNITS+=("$unit")
    fi
    newly_active+=("$unit")
  done <<< "$units"
  ADMISSION_CAPTURED=1
  if [[ "${#newly_active[@]}" -gt 0 ]]; then
    log "closing Velnor admission before drain: ${newly_active[*]}"
    mut systemctl --no-block stop "${newly_active[@]}"
    wait_for_units_stopped "closing Velnor admission" "${newly_active[@]}"
    remaining="$(active_velnor_units)" \
      || die "cannot verify Velnor admission closed; refusing maintenance" 2
    local -a still_active=()
    while IFS= read -r unit; do
      is_admission_unit "$unit" && still_active+=("$unit")
    done <<< "$remaining"
    [[ "${#still_active[@]}" -eq 0 ]] \
      || die "Velnor admission units remained active after stop: ${still_active[*]}" 2
  fi
}

wait_for_work_drain() {
  local containers
  containers="$(container_count_for_maintenance)" \
    || die "Docker container inventory is unknown; refusing maintenance" 2
  if [[ "$containers" -eq 0 ]]; then return 0; fi
  if [[ "$ALLOW_RESTART" == "1" ]]; then
    warn "proceeding with $containers Docker container(s) under explicit VELNOR_C1_ALLOW_RESTART=1"
    return 0
  fi
  die "refusing maintenance with $containers Docker container(s); drain them first" 2
}

ensure_package_lock_file() {
  local lock_dir="${PACKAGE_LOCK_PATH%/*}" created=0
  if [[ -e "$lock_dir" && ! -d "$lock_dir" ]] || [[ -L "$lock_dir" ]]; then
    die "$lock_dir must be a real directory before package maintenance"
  fi
  [[ -d "$lock_dir" ]] || mut install -d -o root -g root -m 0750 "$lock_dir"
  assert_velnor_runtime_directory "$lock_dir" \
    || die "$lock_dir has unsafe ownership or mode"
  if [[ -e "$PACKAGE_LOCK_PATH" || -L "$PACKAGE_LOCK_PATH" ]]; then
    [[ ! -L "$PACKAGE_LOCK_PATH" && -f "$PACKAGE_LOCK_PATH" ]] \
      || die "package transaction lock must be a regular file"
  else
    log "run: create package transaction lock $PACKAGE_LOCK_PATH"
    if (umask 077; set -o noclobber; : > "$PACKAGE_LOCK_PATH") 2>/dev/null; then
      created=1
    fi
    [[ -e "$PACKAGE_LOCK_PATH" && ! -L "$PACKAGE_LOCK_PATH" && -f "$PACKAGE_LOCK_PATH" ]] \
      || die "cannot safely create package transaction lock"
    if [[ "$created" == 1 ]]; then
      chmod 0600 -- "$PACKAGE_LOCK_PATH" \
        || die "cannot set package transaction lock mode"
      chown 0:0 -- "$PACKAGE_LOCK_PATH" \
        || die "cannot set package transaction lock owner"
    fi
  fi
  assert_safe_managed_file "$PACKAGE_LOCK_PATH" "Velnor package transaction lock" \
    || die "package transaction lock has unsafe ownership or mode"
  local lock_metadata lock_owner lock_mode lock_links
  lock_metadata="$(stat -c '%u:%g %a %h' -- "$PACKAGE_LOCK_PATH")" \
    || die "cannot inspect Velnor package transaction lock metadata"
  read -r lock_owner lock_mode lock_links <<< "$lock_metadata"
  [[ "$lock_links" == 1 ]] || die "Velnor package transaction lock must be single-link"
  if [[ "$lock_owner" != 0:0 || "$lock_mode" != 600 ]]; then
    chown 0:0 -- "$PACKAGE_LOCK_PATH" \
      || die "cannot normalize Velnor package transaction lock ownership"
    chmod 0600 -- "$PACKAGE_LOCK_PATH" \
      || die "cannot normalize Velnor package transaction lock mode"
  fi
}

acquire_package_lock() {
  [[ "$PACKAGE_LOCK_HELD" == 1 ]] && return 0
  [[ "${MAINTENANCE_DEADLINE:-0}" != 0 ]] \
    || MAINTENANCE_DEADLINE=$((SECONDS + MAINTENANCE_TIMEOUT))
  ensure_package_lock_file
  exec {PACKAGE_LOCK_FD}<>"$PACKAGE_LOCK_PATH" \
    || die "cannot open package transaction lock"
  local deadline="$MAINTENANCE_DEADLINE"
  local lock_status
  while :; do
    if "$FLOCK_BIN" --exclusive --nonblock "$PACKAGE_LOCK_FD"; then
      break
    else
      lock_status=$?
    fi
    [[ "$lock_status" == 1 ]] \
      || die "failed to acquire exclusive package transaction lock (flock status $lock_status)" 2
    (( SECONDS < deadline )) || die "timed out acquiring exclusive package transaction lock" 2
    close_velnor_admission
    wait_for_work_drain
    sleep 1
  done
  PACKAGE_LOCK_HELD=1
}

ensure_maintenance_barrier() {
  local reason="${1:-C1 host maintenance}"
  [[ "$CHECK" == 0 ]] || return 0
  [[ "$MAINTENANCE_READY" == 1 ]] && return 0
  [[ "${MAINTENANCE_DEADLINE:-0}" != 0 ]] \
    || MAINTENANCE_DEADLINE=$((SECONDS + MAINTENANCE_TIMEOUT))
  log "starting maintenance barrier for $reason"
  close_velnor_admission
  wait_for_work_drain
  acquire_package_lock
  # Recheck after locking: close any reactivated admission unit, then drain again.
  close_velnor_admission
  wait_for_work_drain
  require_drained "$reason"
  MAINTENANCE_READY=1
}

package_read_shared() {
  local shared_fd='' deadline lock_status status
  exec {shared_fd}<>"$PACKAGE_LOCK_PATH" \
    || die "cannot open shared package transaction lock" 2
  deadline="$(maintenance_deadline_value)"
  while :; do
    if "$FLOCK_BIN" --shared --nonblock "$shared_fd"; then
      break
    else
      lock_status=$?
    fi
    if [[ "$lock_status" != 1 ]]; then
      exec {shared_fd}>&-
      die "cannot acquire shared package transaction lock" 2
    fi
    if (( SECONDS >= deadline )); then
      exec {shared_fd}>&-
      die "timed out acquiring shared package transaction lock" 2
    fi
    sleep 1
  done
  if "$@"; then status=0; else status=$?; fi
  "$FLOCK_BIN" --unlock "$shared_fd" 2>/dev/null || true
  exec {shared_fd}>&-
  return "$status"
}

package_read() {
  if [[ "$PACKAGE_LOCK_HELD" == 1 ]]; then
    "$@"
  elif [[ -e "$PACKAGE_LOCK_PATH" || -L "$PACKAGE_LOCK_PATH" ]]; then
    [[ ! -L "$PACKAGE_LOCK_PATH" && -f "$PACKAGE_LOCK_PATH" ]] \
      || die "package transaction lock is unsafe; cannot read package state" 2
    assert_safe_managed_file "$PACKAGE_LOCK_PATH" "Velnor package transaction lock" \
      || die "package transaction lock has unsafe ownership or mode" 2
    package_read_shared "$@"
  elif [[ "$CHECK" -eq 1 && "${1:-}" == dpkg-query ]]; then
    # Status/version reads on a fresh host remain read-only; --check still
    # fails unresolved before an APT transaction or unprotected apt-mark query.
    "$@"
  elif [[ "$CHECK" -eq 1 ]]; then
    die "APT hold state is unresolved: package transaction lock is missing" 2
  else
    if [[ "$VELNOR_LOCK_MISSING" == 1 ]]; then
      ensure_maintenance_barrier "repair the missing Velnor package transaction lock"
    else
      ensure_package_lock_file
    fi
    if [[ "$PACKAGE_LOCK_HELD" == 1 ]]; then "$@"; else package_read_shared "$@"; fi
  fi
}

package_transaction() {
  if [[ "$CHECK" == 1 ]]; then
    mut "$@"
    return 0
  fi
  ensure_maintenance_barrier "APT package transaction"
  [[ "$PACKAGE_LOCK_HELD" == 1 ]] \
    || die "APT transaction is missing the exclusive package lock"
  maintenance_command "$@"
}

release_package_lock() {
  if [[ "$PACKAGE_LOCK_HELD" == 1 && -n "$PACKAGE_LOCK_FD" ]]; then
    "$FLOCK_BIN" --unlock "$PACKAGE_LOCK_FD" 2>/dev/null || true
    exec {PACKAGE_LOCK_FD}>&-
    PACKAGE_LOCK_FD=
    PACKAGE_LOCK_HELD=0
    MAINTENANCE_READY=0
  fi
}

restore_admission_units() {
  if [[ "${#ADMISSION_UNITS[@]}" -gt 0 ]]; then
    if [[ "$DOCKER_CHANGE_STARTED" == 1 && "$DOCKER_LOCAL_HEALTH_VERIFIED" != 1 ]]; then
      die "refusing to reopen Velnor admission before local Docker health is verified"
    fi
    log "reopening prior Velnor admission units: ${ADMISSION_UNITS[*]}"
    ADMISSION_RESTORE_ATTEMPTED=1
    if [[ "$PACKAGE_LOCK_HELD" == 1 ]]; then
      stop_all_active_velnor_units_failsafe
    fi
    release_package_lock
    if ! systemd_available || ! start_admission_units_after_unlock; then
      stop_all_active_velnor_units_failsafe
      return 1
    fi
    ADMISSION_UNITS=()
  fi
  release_package_lock
}

verify_admission_units_active() {
  local state unit
  for unit in "${ADMISSION_UNITS[@]}"; do
    state="$(systemctl show --property=ActiveState --value "$unit" 2>/dev/null)" || return 1
    [[ "$state" == active ]] || return 1
  done
}

start_admission_units_after_unlock() {
  local deadline unit
  [[ "$PACKAGE_LOCK_HELD" == 0 ]] || return 1
  [[ "${#ADMISSION_UNITS[@]}" -gt 0 ]] || return 0
  maintenance_command systemctl --no-block start "${ADMISSION_UNITS[@]}" || return 1
  deadline="$(maintenance_deadline_value)"
  while :; do
    verify_admission_units_active && return 0
    (( SECONDS < deadline )) || return 1
    sleep 1 || return 1
  done
}

preflight_work() {
  log "pre-flight: running-work read"
  local n
  if ! n="$(running_containers)"; then
    warn "running-container inventory is unknown: Docker CLI is missing or docker ps failed"
    WORK_INVENTORY_UNKNOWN=1
    if [[ "$CHECK" -eq 0 ]] && ! docker_runtime_inactive; then
      die "refusing apply because Docker work inventory is unknown and the daemon is not proven stopped" 2
    fi
    return 0
  fi
  log "running containers: $n"
  if [[ "$n" -gt 0 ]]; then
    docker_local ps --format '{{.ID}} {{.Image}} {{.Status}}' 2>/dev/null || true
  fi
  if [[ -d /run/systemd/system ]] && command -v systemctl >/dev/null 2>&1; then
    local units; units="$(active_velnor_units)" \
      || die "cannot inventory Velnor services; maintenance state is unknown" 2
    if [[ -n "$units" ]]; then log "active Velnor units:"; printf '%s\n' "$units"; fi
    local sockets; sockets="$(systemctl list-sockets --all --no-legend --plain 2>/dev/null || true)"
    if [[ -n "$sockets" ]]; then log "systemd sockets:"; printf '%s\n' "$sockets"; fi
  fi
}

assert_safe_managed_file() {
  local path="$1" label="$2" metadata owner mode links mode_bits
  [[ ! -L "$path" && -f "$path" ]] || {
    printf '%s must be a regular non-symlink file: %s\n' "$label" "$path" >&2
    return 2
  }
  metadata="$(stat -c '%u:%g %a %h' -- "$path" 2>/dev/null)" || {
    printf 'cannot read %s ownership, mode, and link count: %s\n' "$label" "$path" >&2
    return 2
  }
  read -r owner mode links <<< "$metadata"
  [[ "$owner" == 0:0 ]] || {
    printf '%s must be owned by root:root: %s\n' "$label" "$path" >&2
    return 2
  }
  [[ "$links" == 1 ]] || {
    printf '%s must not have multiple hard links: %s\n' "$label" "$path" >&2
    return 2
  }
  [[ "$mode" =~ ^[0-7]{3,4}$ ]] || {
    printf '%s has an invalid mode: %s\n' "$label" "$path" >&2
    return 2
  }
  mode_bits=$((8#$mode))
  (( (mode_bits & 0022) == 0 && (mode_bits & 07000) == 0 )) || {
    printf '%s must not be group/world writable or have special permission bits: %s\n' "$label" "$path" >&2
    return 2
  }
}

assert_safe_managed_directory() {
  local path="$1" label="$2" metadata owner mode mode_bits
  [[ ! -L "$path" && -d "$path" ]] || {
    printf '%s must be a real directory: %s\n' "$label" "$path" >&2
    return 2
  }
  metadata="$(stat -c '%u:%g %a' -- "$path" 2>/dev/null)" || {
    printf 'cannot read %s ownership and mode: %s\n' "$label" "$path" >&2
    return 2
  }
  read -r owner mode <<< "$metadata"
  [[ "$owner" == 0:0 ]] || {
    printf '%s must be owned by root:root: %s\n' "$label" "$path" >&2
    return 2
  }
  [[ "$mode" =~ ^[0-7]{3,4}$ ]] || {
    printf '%s has an invalid mode: %s\n' "$label" "$path" >&2
    return 2
  }
  mode_bits=$((8#$mode))
  (( (mode_bits & 0022) == 0 && (mode_bits & 07000) == 0 )) || {
    printf '%s must not be group/world writable or have special permission bits: %s\n' "$label" "$path" >&2
    return 2
  }
}

assert_apt_key_readable_by_sandbox() {
  local path="$1" current mode mode_bits
  assert_safe_managed_file "$path" "Docker APT keyring" || return 2
  mode="$(stat -c '%a' -- "$path" 2>/dev/null)" || return 2
  mode_bits=$((8#$mode))
  (( (mode_bits & 0004) != 0 )) || {
    printf 'Docker APT keyring must be world-readable by the _apt sandbox: %s\n' "$path" >&2
    return 2
  }
  current="${path%/*}"
  while [[ -n "$current" && "$current" != / ]]; do
    assert_safe_managed_directory "$current" "Docker APT keyring parent" || return 2
    mode="$(stat -c '%a' -- "$current" 2>/dev/null)" || return 2
    mode_bits=$((8#$mode))
    (( (mode_bits & 0001) != 0 )) || {
      printf 'Docker APT keyring parent must be traversable by the _apt sandbox: %s\n' "$current" >&2
      return 2
    }
    current="${current%/*}"
  done
}

assert_velnor_runtime_directory() {
  local path="$1" mode
  assert_safe_managed_directory "$path" "Velnor runtime directory" || return 2
  mode="$(stat -c '%a' -- "$path" 2>/dev/null)" || {
    printf 'cannot read Velnor runtime directory mode: %s\n' "$path" >&2
    return 2
  }
  [[ "$mode" == 750 ]] || {
    printf 'Velnor runtime directory must use mode 0750: %s\n' "$path" >&2
    return 2
  }
}

preflight_docker_config() {
  local path=/etc/docker/daemon.json
  log "pre-flight: existing Docker daemon config (read-only)"
  if [[ -e /etc/docker || -L /etc/docker ]]; then
    assert_safe_managed_directory /etc/docker "Docker config directory" \
      || die "unsafe Docker config directory; refusing host changes"
  fi
  if [[ ! -e "$path" && ! -L "$path" ]]; then
    log "daemon.json absent"
    return 0
  fi
  command -v python3 >/dev/null 2>&1 \
    || die "python3 is required to inspect existing daemon.json safely; refusing before host changes"
  [[ ! -L "$path" && -f "$path" ]] \
    || die "daemon.json must be a regular file, not a symlink or special file"
  assert_safe_managed_file "$path" "Docker daemon config" \
    || die "unsafe daemon.json ownership or mode; refusing host changes"
  python3 "$C1_DIR/merge-daemon-json.py" "$path" inspect \
    || die "cannot inspect existing daemon.json safely"
  python3 "$C1_DIR/merge-daemon-json.py" "$path" "$WANT_POOLS" >/dev/null \
    || die "existing daemon.json cannot be safely merged; review it before host changes"
  log "daemon.json is valid and preserves all unmanaged settings"
}

preflight_docker_apt_files() {
  local key_dir="${DOCKER_APT_KEY_FILE%/*}" key_file="$DOCKER_APT_KEY_FILE"
  local source_dir="$DOCKER_APT_SOURCE_DIR"
  local repo_file="$DOCKER_APT_SOURCE_FILE" legacy_file="$DOCKER_APT_LEGACY_SOURCE_FILE"
  local want
  want="$(docker_apt_repo_line)"
  log "pre-flight: Docker APT trust paths"
  if [[ -e "$key_dir" || -L "$key_dir" ]]; then
    assert_safe_managed_directory "$key_dir" "Docker keyring directory" \
      || die "unsafe Docker keyring directory; refusing host changes"
  fi
  if [[ -e "$key_file" || -L "$key_file" ]]; then
    assert_apt_key_readable_by_sandbox "$key_file" \
      || die "unsafe Docker APT keyring ownership or mode; refusing host changes"
  fi
  if [[ -e "$source_dir" || -L "$source_dir" ]]; then
    assert_safe_managed_directory "$source_dir" "APT source directory" \
      || die "unsafe APT source directory; refusing host changes"
  fi
  docker_repo_state "$repo_file" "$legacy_file" "$want" >/dev/null \
    || die "refusing unsafe or unrecognized Docker APT source before host changes"
  reject_unmanaged_docker_sources "$repo_file" "$DOCKER_APT_MAIN_SOURCE_FILE" "$source_dir" \
    || die "another active APT source can supply Docker packages"
}

runner_dpkg_state() { # dpkg-query Status -> installed|absent; reject every partial state
  case "$1" in
    'install ok installed'|'hold ok installed')
      printf 'installed\n'
      ;;
    not-installed|'unknown ok not-installed'|'install ok not-installed'|'deinstall ok not-installed'|\
      'deinstall ok config-files'|'purge ok not-installed')
      printf 'absent\n'
      ;;
    *)
      printf 'unrecognized velnor-runner dpkg status: %s\n' "${1:-<empty>}" >&2
      return 2
      ;;
  esac
}

preflight_docker_runtime() {
  local docker_info
  if docker_info="$(docker_info_snapshot)"; then
    DOCKER_RUNTIME_UNKNOWN=0
    DOCKER_RUNTIME_SNAPSHOT="$docker_info"
    log "Docker: root=${docker_info%%|*} runtime snapshot verified"
  else
    DOCKER_RUNTIME_UNKNOWN=1
    DOCKER_RUNTIME_SNAPSHOT=
    if [[ "$CHECK" -eq 1 ]] || ! docker_runtime_inactive; then
      die "Docker runtime information is unknown (missing Docker CLI or failed docker info)" 2
    fi
    warn "Docker runtime is confirmed stopped before install/start; post-check will require working Docker CLI and docker info"
  fi
}

docker_daemon_config_driver_compatible() {
  local path="$DOCKER_DAEMON_CONFIG_PATH" execstart config_path remaining driver after
  execstart="$(docker_service_execstart)" || return 2
  if [[ "$execstart" != __absent__ ]]; then
    [[ "$execstart" == *dockerd* ]] || {
      printf 'effective docker.service ExecStart is not an inspectable dockerd command\n' >&2
      return 2
    }
    remaining="$execstart"
    if [[ "$remaining" == *--config-file* ]]; then
      remaining="${remaining#*--config-file}"
      if [[ "$remaining" == =* ]]; then
        after="${remaining#=}"
      elif [[ "$remaining" =~ ^[[:space:]] ]]; then
        after="$remaining"
      else
        printf 'effective docker.service ExecStart has an unrecognized --config-file argument\n' >&2
        return 2
      fi
      read -r config_path _ <<< "$after"
      config_path="${config_path%\"}"
      config_path="${config_path#\"}"
      [[ -n "$config_path" && "$remaining" != *--config-file* ]] || {
        printf 'effective docker.service ExecStart repeats --config-file\n' >&2
        return 2
      }
      [[ "$config_path" == "$DOCKER_DAEMON_CONFIG_PATH" ]] || {
        printf 'effective docker.service uses unmanaged daemon config: %s\n' "$config_path" >&2
        return 1
      }
    fi
    remaining="$execstart"
    while [[ "$remaining" == *native.cgroupdriver=* ]]; do
      remaining="${remaining#*native.cgroupdriver=}"
      [[ "$remaining" =~ ^([[:alnum:]_-]+) ]] || {
        printf 'effective docker.service ExecStart has a malformed cgroup driver option\n' >&2
        return 2
      }
      driver="${BASH_REMATCH[1]}"
      [[ "$driver" == systemd ]] || {
        printf 'effective docker.service ExecStart selects cgroup driver %s\n' "${driver:-unknown}" >&2
        return 1
      }
      remaining="${remaining#"$driver"}"
    done
  fi
  [[ -f "$path" ]] || return 0
  python3 - "$path" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as stream:
    config = json.load(stream)
options = config.get("exec-opts", [])
if not isinstance(options, list) or any(not isinstance(option, str) for option in options):
    raise SystemExit(2)
drivers = [option.split("=", 1)[1] for option in options if option.startswith("native.cgroupdriver=")]
if any(driver != "systemd" for driver in drivers):
    raise SystemExit(1)
PY
}

docker_service_execstart() {
  local load_state execstart
  systemd_available || return 2
  load_state="$(systemctl show --property=LoadState --value docker.service 2>/dev/null)" \
    || return 2
  case "$load_state" in
    not-found) printf '__absent__\n'; return 0 ;;
    loaded) ;;
    *) printf 'unknown docker.service LoadState: %s\n' "${load_state:-empty}" >&2; return 2 ;;
  esac
  execstart="$(systemctl show --property=ExecStart --value docker.service 2>/dev/null)" \
    || return 2
  [[ -n "$execstart" ]] || {
    printf 'effective docker.service ExecStart is empty\n' >&2
    return 2
  }
  printf '%s\n' "$execstart"
}

host_has_libvirt_or_qemu() {
  command -v virsh >/dev/null 2>&1 \
    || dpkg -l 2>/dev/null | grep -Eq 'libvirt|qemu-kvm|qemu-system'
}

preflight_host_invariants() {
  local root storage logging driver cgroup
  preflight_systemd
  [[ -f "$DOCKER_CGROUP_MARKER_PATH" ]] \
    || die "cgroup v2 is required; refusing host changes before package or config mutation" 2
  if [[ "$DOCKER_RUNTIME_UNKNOWN" == 0 ]]; then
    IFS='|' read -r root storage logging driver cgroup <<< "$DOCKER_RUNTIME_SNAPSHOT"
    [[ "$driver" == systemd ]] \
      || die "Docker cgroup driver is $driver; expected systemd before host changes" 2
    [[ "$cgroup" == 2 ]] \
      || die "Docker reports cgroup v$cgroup; cgroup v2 is required before host changes" 2
  fi
  docker_daemon_config_driver_compatible \
    || die "effective docker.service ExecStart or daemon config selects an unsupported or unresolved cgroup driver/config" 2
  host_has_libvirt_or_qemu \
    && die "libvirt/KVM/QEMU is present; refusing host changes before APT/config mutation" 2
  log "read-only host admission passed: cgroup v2, systemd Docker cgroup driver, no libvirt/KVM/QEMU"
}

preflight_firewall() {
  log "pre-flight: firewall (read-only; no rule changes)"
  if command -v nft >/dev/null 2>&1; then
    nft list ruleset 2>&1 || warn "nft ruleset read failed"
  elif command -v iptables-save >/dev/null 2>&1; then
    iptables-save 2>&1 || warn "iptables ruleset read failed"
  elif command -v ufw >/dev/null 2>&1; then
    ufw status verbose 2>&1 || warn "ufw status read failed"
  else
    warn "no nft, iptables-save, or ufw; firewall state requires operator review"
  fi
}

preflight_runner_lock() {
  local runner_status runner_query_status=0 runner_state
  if [[ -e "$PACKAGE_LOCK_PATH" || -L "$PACKAGE_LOCK_PATH" ]]; then
    assert_velnor_runtime_directory "${PACKAGE_LOCK_PATH%/*}" \
      || die "/run/velnor has unsafe ownership or mode"
    assert_safe_managed_file "$PACKAGE_LOCK_PATH" "Velnor package transaction lock" \
      || die "Velnor package transaction lock has unsafe ownership or mode"
    runner_status="$(package_read dpkg-query -W -f='${Status}' velnor-runner 2>/dev/null)" \
      || runner_query_status=$?
  else
    # Detect a broken installed Velnor setup before package_read can create a
    # fresh lock for this bootstrap invocation.
    runner_status="$(dpkg-query -W -f='${Status}' velnor-runner 2>/dev/null)" \
      || runner_query_status=$?
  fi
  case "$runner_query_status" in
    0) : ;;
    1)
      if [[ -z "$runner_status" ]]; then
        runner_status=not-installed
      else
        runner_state="$(runner_dpkg_state "$runner_status")" \
          || die "Velnor package state is unresolved: ${runner_status:-empty status}" 2
        [[ "$runner_state" == absent ]] \
          || die "cannot safely query velnor-runner package state" 2
      fi
      ;;
    *) die "cannot safely query velnor-runner package state" 2 ;;
  esac
  runner_state="$(runner_dpkg_state "$runner_status")" \
    || die "Velnor package state is unresolved: ${runner_status:-empty status}" 2
  if [[ "$runner_state" == installed ]]; then
    RUNNER_INSTALLED=1
    if [[ -f "$PACKAGE_LOCK_PATH" && ! -L "$PACKAGE_LOCK_PATH" ]]; then
      log "Velnor package transaction lock is present"
    elif [[ "$CHECK" == 1 ]]; then
      die "Velnor package state is unresolved: installed velnor-runner has no package transaction lock" 2
    else
      VELNOR_LOCK_MISSING=1
      warn "installed velnor-runner lacks its runtime lock; apply will repair it under the maintenance barrier"
    fi
  fi
}

collect_stock_daemon_instances() {
  local unit_files loaded_units unit state rest instance env_file stem
  local -A seen=()
  DAEMON_INSTANCES=()
  systemd_available \
    || die "cannot resolve Velnor daemon units without systemd" 2
  unit_files="$(maintenance_command systemctl list-unit-files --type=service \
    --no-legend --no-pager 'velnor-daemon@*.service' 2>/dev/null)" \
    || die "cannot inventory configured Velnor daemon unit files" 2
  loaded_units="$(maintenance_command systemctl list-units --all --plain \
    --no-legend --no-pager 'velnor-daemon@*.service' 2>/dev/null)" \
    || die "cannot inventory loaded Velnor daemon units" 2
  while read -r unit state rest; do
    [[ "$unit" == velnor-daemon@*.service && "$unit" != velnor-daemon@.service ]] || continue
    instance="${unit#velnor-daemon@}"
    instance="${instance%.service}"
    [[ "$instance" =~ ^[A-Za-z0-9_.-]+$ ]] \
      || die "unsupported Velnor daemon instance unit: $unit" 2
    [[ -f "/etc/velnor/$instance.env" && ! -L "/etc/velnor/$instance.env" ]] \
      || die "$unit has no regular /etc/velnor/$instance.env" 2
    seen["$instance"]=1
  done <<< "$unit_files"
  while read -r unit state rest; do
    [[ "$unit" == velnor-daemon@*.service && "$unit" != velnor-daemon@.service ]] || continue
    instance="${unit#velnor-daemon@}"
    instance="${instance%.service}"
    [[ "$instance" =~ ^[A-Za-z0-9_.-]+$ ]] \
      || die "unsupported loaded Velnor daemon instance unit: $unit" 2
    [[ -f "/etc/velnor/$instance.env" && ! -L "/etc/velnor/$instance.env" ]] \
      || die "$unit has no regular /etc/velnor/$instance.env" 2
    seen["$instance"]=1
  done <<< "$loaded_units"
  if [[ -d /etc/velnor && ! -L /etc/velnor ]]; then
    for env_file in /etc/velnor/*.env; do
      [[ -e "$env_file" || -L "$env_file" ]] || continue
      stem="${env_file##*/}"
      stem="${stem%.env}"
      [[ "$stem" != velnor && "$stem" != secrets && "$stem" != *.* ]] || continue
      [[ "$stem" =~ ^[A-Za-z0-9_.-]+$ ]] \
        || die "unsupported Velnor environment filename: $env_file" 2
      seen["$stem"]=1
    done
  fi
  if [[ "${#seen[@]}" -gt 0 ]]; then
    mapfile -t DAEMON_INSTANCES < <(printf '%s\n' "${!seen[@]}" | LC_ALL=C sort)
  fi
}

assert_stock_daemon_unit() {
  local unit="$1" expected_fragment="$2" expected_environment="$3"
  local load_state fragment dropins execstart environment unit_dir secret_environment environment_lines expected_sha256 actual_sha256 binding
  load_state="$(maintenance_command systemctl show --property=LoadState --value "$unit" 2>/dev/null)" \
    || die "cannot inspect $unit load state" 2
  [[ "$load_state" == loaded ]] || die "$unit is not a loaded packaged service" 2
  fragment="$(maintenance_command systemctl show --property=FragmentPath --value "$unit" 2>/dev/null)" \
    || die "cannot inspect $unit fragment path" 2
  local fragment_is_vendor=0
  for unit_dir in "${SYSTEMD_VENDOR_UNIT_DIRS[@]}"; do
    if [[ "$fragment" == "$unit_dir/$expected_fragment" ]]; then
      fragment_is_vendor=1
      break
    fi
  done
  [[ "$fragment_is_vendor" == 1 ]] \
    || die "$unit uses an unmanaged systemd fragment: ${fragment:-empty}" 2
  [[ -f "$fragment" && ! -L "$fragment" ]] \
    || die "$unit fragment is missing or not a regular file: $fragment" 2
  assert_safe_managed_file "$fragment" "$unit fragment" \
    || die "$unit fragment has unsafe metadata" 2
  case "$expected_fragment" in
    velnor-daemon.service) expected_sha256="$STOCK_DAEMON_UNIT_SHA256" ;;
    velnor-daemon@.service) expected_sha256="$STOCK_DAEMON_TEMPLATE_UNIT_SHA256" ;;
    *) die "unsupported stock Velnor daemon fragment: $expected_fragment" 2 ;;
  esac
  actual_sha256="$(sha256sum -- "$fragment" | awk '{print $1}')" \
    || die "cannot fingerprint $unit fragment" 2
  [[ "$actual_sha256" == "$expected_sha256" ]] \
    || die "$unit fragment differs from the packaged path-and-slot contract" 2
  dropins="$(maintenance_command systemctl show --property=DropInPaths --value "$unit" 2>/dev/null)" \
    || die "cannot inspect $unit drop-ins" 2
  [[ -z "$dropins" ]] || die "$unit has systemd drop-ins; effective daemon paths are unresolved" 2
  execstart="$(maintenance_command systemctl show --property=ExecStart --value "$unit" 2>/dev/null)" \
    || die "cannot inspect $unit effective ExecStart" 2
  [[ "$execstart" == *"/usr/bin/velnor-runner daemon"* ]] \
    || die "$unit does not use the stock Velnor daemon executable" 2
  [[ "$execstart" == *"/usr/bin/flock --shared --no-fork /run/velnor/package-transaction.lock"* ]] \
    || die "$unit does not wait for the Velnor package transaction lock" 2
  for binding in \
    '--url ${VELNOR_URL}' \
    '--name ${VELNOR_NAME}' \
    '--slots ${VELNOR_SLOTS}' \
    '--work-dir ${VELNOR_WORK_DIR}'; do
    [[ "$execstart" == *"$binding"* ]] \
      || die "$unit does not bind its daemon paths/capacity to $binding" 2
  done
  case "$execstart" in
    *--state-db*|*--permit-ledger*|*--scale-set-config*|*--config-dir*|*--storage-root*)
      die "$unit passes daemon storage paths outside the stock environment contract" 2
      ;;
  esac
  environment="$(maintenance_command systemctl show --property=Environment --value "$unit" 2>/dev/null)" \
    || die "cannot inspect $unit effective environment" 2
  case "$environment" in
    *VELNOR_STATE_DB=*|*VELNOR_PERMIT_LEDGER=*|*VELNOR_SCALE_SET_CONFIG=*|*VELNOR_CONFIG_DIR=*|*VELNOR_NAME=*|*VELNOR_WORK_DIR=*|*VELNOR_URL=*|*VELNOR_SLOTS=*)
      die "$unit overrides daemon path, identity, or capacity values outside its environment file" 2
      ;;
  esac
  if [[ "$expected_fragment" == velnor-daemon.service ]]; then
    secret_environment=/etc/velnor/secrets.env
  else
    secret_environment=/etc/velnor/%i.secrets.env
  fi
  environment_lines="$(LC_ALL=C grep '^EnvironmentFile=' "$fragment" | LC_ALL=C sort)" \
    || die "$unit fragment does not declare stock environment files" 2
  [[ "$environment_lines" == "$(printf 'EnvironmentFile=%s\nEnvironmentFile=-%s\n' \
    "$expected_environment" "$secret_environment" | LC_ALL=C sort)" ]] \
    || die "$unit does not load only the stock Velnor environment files" 2
}

step_permit_ledger_roster() {
  [[ "$RUNNER_INSTALLED" == 1 ]] || {
    log "permit-source roster skipped: velnor-runner is not installed"
    return 0
  }
  local unit instance
  local -a helper_args=(--stock-daemon --root "$PERMIT_LEDGER_ROSTER_ROOT")
  assert_stock_daemon_unit velnor-daemon.service velnor-daemon.service \
    /etc/velnor/velnor.env
  collect_stock_daemon_instances
  for instance in "${DAEMON_INSTANCES[@]}"; do
    unit="velnor-daemon@$instance.service"
    assert_stock_daemon_unit "$unit" velnor-daemon@.service /etc/velnor/%i.env
    helper_args+=(--instance "$instance")
  done
  if [[ "$CHECK" == 1 ]]; then
    helper_args+=(--check)
  else
    ensure_maintenance_barrier "provision the Velnor permit-source roster and state paths"
    require_drained "provision the Velnor permit-source roster and state paths"
    [[ "$PACKAGE_LOCK_HELD" == 1 ]] \
      || die "permit-source roster provisioning requires the exclusive package transaction lock" 2
    [[ "$PACKAGE_LOCK_FD" =~ ^[0-9]+$ ]] \
      || die "permit-source roster provisioning has no inherited package-lock descriptor" 2
    helper_args+=(--package-lock-fd "$PACKAGE_LOCK_FD")
  fi
  maintenance_command python3 "$C1_DIR/provision-permit-ledger-roster.py" "${helper_args[@]}" \
    || die "Velnor permit-source roster or required state paths failed validation" 2
}

preflight_env() {
  local node
  log "pre-flight: CPU, NUMA, RAM, storage, sockets, and daemon environment (read-only)"
  if command -v lscpu >/dev/null 2>&1; then
    lscpu
  else
    getconf _NPROCESSORS_ONLN 2>/dev/null || warn "CPU count unavailable"
    awk -F: '/^(model name|processor)[[:space:]]*:/ {print}' /proc/cpuinfo 2>/dev/null || true
  fi
  if command -v numactl >/dev/null 2>&1; then
    numactl --hardware 2>&1 || warn "NUMA read failed"
  elif compgen -G '/sys/devices/system/node/node[0-9]*' >/dev/null; then
    ls -d /sys/devices/system/node/node[0-9]* 2>/dev/null || true
    for node in /sys/devices/system/node/node[0-9]*; do
      [[ -r "$node/cpulist" ]] && printf '%s cpulist: %s\n' "$node" "$(<"$node/cpulist")"
      [[ -r "$node/meminfo" ]] && cat "$node/meminfo"
    done
  else
    log "single NUMA node or NUMA topology unavailable"
  fi
  if command -v free >/dev/null 2>&1; then free -h; else cat /proc/meminfo; fi
  if [[ -f /sys/fs/cgroup/cgroup.controllers ]]; then
    log "cgroup v2 present"
  else
    warn "cgroup v2 NOT detected (verify-only here; post-check re-tests)"
  fi
  log "APT sources:"
  ls /etc/apt/sources.list.d/ 2>/dev/null || true
  [[ -f /etc/apt/sources.list ]] && cat /etc/apt/sources.list || true
  log "block devices and filesystems (read-only; script never touches storage):"
  if command -v lsblk >/dev/null 2>&1; then lsblk -e7 -o NAME,SIZE,TYPE,FSTYPE,RO,MOUNTPOINTS 2>/dev/null || true; fi
  if command -v findmnt >/dev/null 2>&1; then findmnt --real --output TARGET,SOURCE,FSTYPE,OPTIONS 2>/dev/null || true; fi
  if command -v blkid >/dev/null 2>&1; then blkid -o full 2>/dev/null || true; fi
  if command -v df >/dev/null 2>&1; then
    df -hT
    df -ih
  fi
  if command -v ss >/dev/null 2>&1; then
    log "listening and active sockets:"
    ss -pluntx 2>&1 || warn "socket read failed"
  else
    warn "ss unavailable; socket inventory requires operator review"
  fi
  preflight_firewall
  preflight_docker_config
  preflight_docker_apt_files
  preflight_active_apt_sources \
    || die "active APT sources are outside the approved Debian/Docker repositories" 2
  preflight_runner_lock
  preflight_docker_runtime
}

# Drain gate used only after admission closes and the package lock is held.
# VELNOR_C1_ALLOW_RESTART permits explicitly accepted non-Velnor containers.
require_drained() {
  local reason="$1"
  local n
  n="$(container_count_for_maintenance)" \
    || die "refusing to $reason because Docker work inventory cannot be proven" 2
  if [[ "$n" -gt 0 && "$ALLOW_RESTART" != "1" ]]; then
    die "refusing to $reason with $n running container(s); drain jobs first, then re-run with VELNOR_C1_ALLOW_RESTART=1" 2
  fi
  if [[ "$n" -gt 0 ]]; then
    warn "proceeding to $reason with $n running container(s) (VELNOR_C1_ALLOW_RESTART=1)"
  fi
}

# ------------------------------------------------------------------- helpers

pkg_installed() { # pkg_installed <pkg> [<version>] -> 0 iff installed (and version matches)
  local pkg="$1" want="${2:-}" status ver query_status=0
  status="$(package_read dpkg-query -W -f='${Status}' "$pkg" 2>/dev/null)" \
    || query_status=$?
  case "$query_status" in
    0) : ;;
    1) status="not-installed" ;;
    *) die "cannot safely query package state for $pkg" 2 ;;
  esac
  # NB: held packages report "hold ok installed" — accept both desired states.
  case "$status" in
    'install ok installed'|'hold ok installed') ;;
    not-installed|'unknown ok not-installed'|'install ok not-installed'|\
      'deinstall ok not-installed'|'deinstall ok config-files'|'purge ok not-installed')
      return 1
      ;;
    *) die "package state is unresolved for $pkg: ${status:-empty status}" 2 ;;
  esac
  if [[ -n "$want" ]]; then
    query_status=0
    ver="$(package_read dpkg-query -W -f='${Version}' "$pkg" 2>/dev/null)" \
      || query_status=$?
    [[ "$query_status" -le 1 ]] \
      || die "cannot safely query installed version for $pkg" 2
    [[ "$ver" == "$want" ]] || return 1
  fi
  return 0
}

assert_installed_package_not_newer_than_pin() {
  local pkg="$1" want="$2" status current query_status=0 comparison_status=0
  status="$(package_read dpkg-query -W -f='${Status}' "$pkg" 2>/dev/null)" \
    || query_status=$?
  case "$query_status:$status" in
    0:'install ok installed'|0:'hold ok installed') ;;
    1:*|0:not-installed|0:'unknown ok not-installed'|0:'install ok not-installed'|\
      0:'deinstall ok not-installed'|0:'deinstall ok config-files'|0:'purge ok not-installed')
      return 0
      ;;
    *) die "package state is unresolved for $pkg: ${status:-empty status}" 2 ;;
  esac
  current="$(package_read dpkg-query -W -f='${Version}' "$pkg" 2>/dev/null)" \
    || die "cannot safely query installed version for $pkg" 2
  [[ -n "$current" ]] || die "installed version is unresolved for $pkg" 2
  if dpkg --compare-versions "$current" gt "$want"; then
    die "installed $pkg version $current is newer than requested pin $want; refusing an unapproved downgrade" 2
  else
    comparison_status=$?
  fi
  [[ "$comparison_status" == 1 ]] \
    || die "cannot compare installed $pkg version $current with requested pin $want" 2
}

APT_UPDATED=0
apt_update_once() {
  if [[ "$APT_UPDATED" -eq 0 ]]; then
    package_transaction apt_command apt-get update
    APT_UPDATED=1
  fi
}

# ensure_present_pkgs: install only missing requested packages. The reviewed
# resolver plan rejects any version change to an already-installed dependency.
ensure_present_pkgs() {
  local missing=() p
  for p in "$@"; do
    pkg_installed "$p" || missing+=("$p")
  done
  if [[ "${#missing[@]}" -eq 0 ]]; then
    log "all present: $*"
    return 0
  fi
  if [[ "$CHECK" == 1 ]]; then
    log "would install missing packages: ${missing[*]}"
    die "APT resolution unresolved in --check for ${missing[*]}; no APT list refresh or resolver simulation was performed" 2
  fi
  ensure_maintenance_barrier "install ${missing[*]}"
  require_drained "install ${missing[*]} before APT list refresh"
  apt_update_once
  require_drained "install ${missing[*]} after APT list refresh"
  verify_install_plan_origins --reject-installed-upgrades --no-upgrade --no-remove "${missing[@]}"
  preflight_active_apt_sources \
    || die "active APT sources changed to an unapproved origin before package installation" 2
  require_drained "install ${missing[*]} immediately before package transaction"
  preflight_active_apt_sources \
    || die "active APT sources changed after resolver review" 2
  package_transaction apt_plan_guarded_install install -y --no-upgrade --no-remove "${missing[@]}"
}

current_timezone() {
  if command -v timedatectl >/dev/null 2>&1; then
    local tz; tz="$(timedatectl show -p Timezone --value 2>/dev/null || true)"
    if [[ -n "$tz" ]]; then printf '%s' "$tz"; return 0; fi
  fi
  if [[ -L /etc/localtime ]]; then
    readlink /etc/localtime | sed 's|.*/zoneinfo/||'
    return 0
  fi
  if [[ -f /etc/timezone ]]; then cat /etc/timezone; return 0; fi
  printf 'unknown'
}

# ---------------------------------------------------------------------- core

step_timezone() { # B1
  log "step: timezone UTC (B1)"
  if [[ "$(current_timezone)" == "UTC" ]]; then
    log "already UTC"
    return 0
  fi
  if [[ -d /run/systemd/system ]] && command -v timedatectl >/dev/null 2>&1; then
    mut timedatectl set-timezone UTC
  else
    mut ln -sf /usr/share/zoneinfo/UTC /etc/localtime
    mut bash -c 'echo UTC > /etc/timezone'
  fi
}

step_base_packages() { # B2 (+ C1 prereqs): curl/gnupg for key verification, python3 for safe JSON merge
  log "step: base APT set, present-only (B2+C1)"
  ensure_present_pkgs ca-certificates curl gnupg python3
  ensure_present_pkgs gpg sudo wget curl zsh git git-lfs unzip tmux iotop bat \
    ncurses-term build-essential pkg-config libssl-dev
  ensure_present_pkgs apt-transport-https lsb-release
}

step_git_lfs_bat() { # B4
  log "step: git-lfs + bat symlink (B4)"
  # Guarded (unlike the source's always-changed): skip when LFS filters exist.
  if [[ -n "$(git config --global --get filter.lfs.process 2>/dev/null || true)" ]]; then
    log "git-lfs already initialized"
  else
    mut git lfs install
  fi
  if [[ -L /usr/local/bin/bat ]]; then
    log "bat symlink already present"
  elif [[ ! -e /usr/bin/batcat ]]; then
    warn "no /usr/bin/batcat; skipping bat symlink"
  else
    mut ln -s /usr/bin/batcat /usr/local/bin/bat
  fi
}

key_fingerprint_records() { # key_fingerprint_records <file> -> exact typed pub/sub set
  gpg --show-keys --with-colons -- "$1" 2>/dev/null | awk -F: '
    $1 == "pub" || $1 == "sub" {
      if (pending != "") exit 2
      pending = $1
      next
    }
    $1 == "fpr" {
      if (pending == "") exit 3
      printf "%s:%s\n", pending, toupper($10)
      pending = ""
    }
    END { if (pending != "") exit 4 }
  '
}

step_docker_key() { # C2, fail-closed fingerprint authentication
  log "step: docker APT key, fingerprinted (C2)"
  if ! command -v curl >/dev/null 2>&1 || ! command -v gpg >/dev/null 2>&1; then
    if [[ "$CHECK" -eq 1 ]]; then
      # Bootstrap packages are themselves pending (see B2 step above); the
      # live key fetch+verify cannot run until they exist.
      PENDING=$((PENDING + 1))
      printf '    would: fetch + fingerprint-verify docker key (deferred: curl/gnupg not installed yet)\n'
      return 0
    fi
    die "curl/gnupg missing in apply mode after bootstrap; refusing to continue"
  fi
  local tmp; tmp="$(mktemp)"
  track_temp "$tmp"
  if [[ "$CHECK" -eq 1 ]]; then
    # Check mode still fetches+verifies (read-only network) so a bad key fails fast.
    log "run: curl -fsSL $VELNOR_C1_DOCKER_KEY_URL (verify-only)"
    curl -fsSL "$VELNOR_C1_DOCKER_KEY_URL" -o "$tmp"
  else
    log "run: curl -fsSL $VELNOR_C1_DOCKER_KEY_URL"
    curl -fsSL "$VELNOR_C1_DOCKER_KEY_URL" -o "$tmp"
  fi
  local got expected
  if ! got="$(key_fingerprint_records "$tmp")"; then
    die "could not parse Docker key fingerprint records; refusing installation"
  fi
  expected="$(printf 'pub:%s\nsub:%s' \
    "$VELNOR_C1_DOCKER_KEY_FPR" "$VELNOR_C1_DOCKER_KEY_SUB")"
  [[ "$got" == "$expected" ]] \
    || die "Docker key fingerprint set mismatch: got [${got//$'\n'/, }], want exactly [${expected//$'\n'/, }]"
  log "complete Docker key fingerprint set authenticated: ${VELNOR_C1_DOCKER_KEY_FPR} + ${VELNOR_C1_DOCKER_KEY_SUB}"
  if [[ -e /etc/apt/keyrings || -L /etc/apt/keyrings ]]; then
    assert_safe_managed_directory /etc/apt/keyrings "Docker keyring directory" \
      || die "unsafe Docker keyring directory; refusing key installation"
  fi
  local key_state
  key_state="$(docker_key_state /etc/apt/keyrings/docker.asc "$tmp")" \
    || die "existing Docker keyring is not safely managed; review it before host changes"
  if [[ "$key_state" == current ]]; then
    log "keyring already current"
    return 0
  fi
  if [[ "$CHECK" == 1 ]]; then
    PENDING=$((PENDING + 1))
    printf '    would: install authenticated Docker key at /etc/apt/keyrings/docker.asc\n'
    return 0
  fi
  ensure_maintenance_barrier "install Docker APT key"
  [[ -d /etc/apt/keyrings ]] || mut install -o root -g root -m 0755 -d /etc/apt/keyrings
  mut install -m 0644 "$tmp" /etc/apt/keyrings/docker.asc
}

docker_key_state() { # path, authenticated download -> current|missing; reject all unsafe existing state
  local path="$1" verified="$2"
  if [[ ! -e "$path" && ! -L "$path" ]]; then
    printf 'missing\n'
    return 0
  fi
  assert_apt_key_readable_by_sandbox "$path" || return 2
  if cmp -s -- "$verified" "$path"; then
    printf 'current\n'
    return 0
  fi
  printf 'installed Docker key differs from verified download\n' >&2
  return 2
}

step_docker_repo() { # C2 repo file
  log "step: docker APT repo (C2)"
  local want="deb [arch=amd64 signed-by=/etc/apt/keyrings/docker.asc] ${VELNOR_C1_DOCKER_REPO_URL} ${VELNOR_C1_DOCKER_DIST} ${VELNOR_C1_DOCKER_COMPONENT}"
  local repo_file=/etc/apt/sources.list.d/docker.list
  local legacy_file=/etc/apt/sources.list.d/docker-ce.list repo_state
  repo_state="$(docker_repo_state "$repo_file" "$legacy_file" "$want")" \
    || die "refusing to overwrite or delete an unrecognized Docker APT source; review it manually"
  if [[ "$repo_state" == current ]]; then
    log "repo file already current"
  else
    if [[ "$CHECK" == 0 ]]; then
      ensure_maintenance_barrier "write Docker APT source"
    else
      PENDING=$((PENDING + 1))
      printf '    would: create Docker APT source %s\n' "$repo_file"
      APT_UPDATED=0
      return 0
    fi
    mut write_new_apt_repo "$want" "$repo_file"
    APT_UPDATED=0 # repo changed -> refresh before installs
  fi
}

docker_repo_state() { # path, legacy path, exact expected content -> current|missing
  local repo_file="$1" legacy_file="$2" want="$3" source_dir
  source_dir="$(dirname -- "$repo_file")"
  if [[ -e "$source_dir" || -L "$source_dir" ]]; then
    assert_safe_managed_directory "$source_dir" "APT source directory" || return 2
  fi
  if [[ -e "$legacy_file" || -L "$legacy_file" ]]; then
    printf 'legacy Docker APT source exists: %s\n' "$legacy_file" >&2
    return 2
  fi
  if [[ ! -e "$repo_file" && ! -L "$repo_file" ]]; then
    printf 'missing\n'
    return 0
  fi
  assert_safe_managed_file "$repo_file" "Docker APT source" || return 2
  if cmp -s <(printf '%s\n' "$want") "$repo_file"; then
    printf 'current\n'
    return 0
  fi
  printf 'Docker APT source has unrecognized content: %s\n' "$repo_file" >&2
  return 2
}

docker_apt_repo_line() {
  printf 'deb [arch=amd64 signed-by=%s] %s %s %s\n' \
    "$DOCKER_APT_KEY_FILE" "$VELNOR_C1_DOCKER_REPO_URL" \
    "$VELNOR_C1_DOCKER_DIST" "$VELNOR_C1_DOCKER_COMPONENT"
}

apt_source_entries() { # active deb entries -> URI<TAB>suite<TAB>component<TAB>Signed-By<TAB>unsafe-trust
  local source_file="$1"
  case "$source_file" in
    *.list)
      awk '
        /^[[:space:]]*#/ { next }
        /^[[:space:]]*deb(-src)?[[:space:]]/ {
          line = $0
          kind = line
          sub(/^[[:space:]]*/, "", kind)
          sub(/[[:space:]].*/, "", kind)
          if (kind != "deb") next
          sub(/^[[:space:]]*deb[[:space:]]+/, "", line)
          options = ""
          if (line ~ /^\[/) {
            match(line, /^\[[^]]*\]/)
            options = substr(line, 2, RLENGTH - 2)
            line = substr(line, RLENGTH + 1)
          }
          sub(/^[[:space:]]+/, "", line)
          count = split(line, fields, /[[:space:]]+/)
          signed_by = ""
          unsafe = ""
          option_count = split(options, option, /[[:space:]]+/)
          for (i = 1; i <= option_count; i++) {
            option_key = tolower(option[i])
            if (option_key ~ /^signed-by=/) {
              signed_by = option[i]
              sub(/^[^=]*=/, "", signed_by)
            }
            if (option_key ~ /^(trusted|allow-insecure|allow-weak|allow-downgrade-to-insecure)=/) {
              option_value = option_key
              sub(/^[^=]*=/, "", option_value)
              if (option_value != "no" && option_value != "false" && option_value != "0") unsafe = "yes"
            }
          }
          if (fields[1] != "" && fields[2] != "") {
            if (fields[2] == "./" || count < 3) {
              printf "%s\t%s\t__flat_repository__\t%s\t%s\n", fields[1], fields[2], signed_by, unsafe
            } else {
              for (i = 3; i <= count; i++)
                if (fields[i] != "") printf "%s\t%s\t%s\t%s\t%s\n", fields[1], fields[2], fields[i], signed_by, unsafe
            }
          }
        }
      ' "$source_file"
      ;;
    *.sources)
      awk '
        function emit(    uri_count, suite_count, component_count, i, j, k, unsafe) {
          if (types !~ /(^|[[:space:]])deb([[:space:]]|$)/ || tolower(enabled) == "no") return
          unsafe = ""
          if (tolower(trusted) != "" && tolower(trusted) != "no" && tolower(trusted) != "false" && tolower(trusted) != "0") unsafe = "yes"
          if (tolower(allow_insecure) != "" && tolower(allow_insecure) != "no" && tolower(allow_insecure) != "false" && tolower(allow_insecure) != "0") unsafe = "yes"
          if (tolower(allow_weak) != "" && tolower(allow_weak) != "no" && tolower(allow_weak) != "false" && tolower(allow_weak) != "0") unsafe = "yes"
          if (tolower(allow_downgrade) != "" && tolower(allow_downgrade) != "no" && tolower(allow_downgrade) != "false" && tolower(allow_downgrade) != "0") unsafe = "yes"
          uri_count = split(uris, uri, /[[:space:]]+/)
          suite_count = split(suites, suite, /[[:space:]]+/)
          component_count = split(components, component, /[[:space:]]+/)
          for (i = 1; i <= uri_count; i++) {
            for (j = 1; j <= suite_count; j++) {
              if (uri[i] != "" && suite[j] != "" && (suite[j] == "./" || component_count == 0 || component[1] == "")) {
                printf "%s\t%s\t__flat_repository__\t%s\t%s\n", uri[i], suite[j], signed_by, unsafe
              } else {
                for (k = 1; k <= component_count; k++) {
                  if (uri[i] != "" && suite[j] != "" && component[k] != "")
                  printf "%s\t%s\t%s\t%s\t%s\n", uri[i], suite[j], component[k], signed_by, unsafe
                }
              }
            }
          }
        }
        /^[[:space:]]*#/ { next }
        /^[[:space:]]*$/ {
          emit()
          types = ""; uris = ""; suites = ""; components = ""; enabled = ""; signed_by = ""; trusted = ""; allow_insecure = ""; allow_weak = ""; allow_downgrade = ""; field = ""
          next
        }
        /^[^[:space:]][^:]*:/ {
          field = tolower($0)
          sub(/:.*/, "", field)
          value = $0
          sub(/^[^:]*:[[:space:]]*/, "", value)
          if (field == "types") types = value
          if (field == "uris") uris = value
          if (field == "suites") suites = value
          if (field == "components") components = value
          if (field == "enabled") enabled = tolower(value)
          if (field == "signed-by") signed_by = value
          if (field == "trusted") trusted = value
          if (field == "allow-insecure") allow_insecure = value
          if (field == "allow-weak") allow_weak = value
          if (field == "allow-downgrade-to-insecure") allow_downgrade = value
          next
        }
        /^[[:space:]]+/ {
          value = $0
          sub(/^[[:space:]]+/, "", value)
          if (field == "types") types = types " " value
          if (field == "uris") uris = uris " " value
          if (field == "suites") suites = suites " " value
          if (field == "components") components = components " " value
          if (field == "enabled") enabled = enabled " " tolower(value)
          if (field == "signed-by") signed_by = signed_by " " value
          if (field == "trusted") trusted = trusted " " value
          if (field == "allow-insecure") allow_insecure = allow_insecure " " value
          if (field == "allow-weak") allow_weak = allow_weak " " value
          if (field == "allow-downgrade-to-insecure") allow_downgrade = allow_downgrade " " value
        }
        END { emit() }
      ' "$source_file"
      ;;
  esac
}

normalize_apt_uri() {
  local uri="$1" scheme remainder authority path
  [[ "$uri" == *://* ]] || return 1
  scheme="${uri%%://*}"
  remainder="${uri#*://}"
  authority="${remainder%%/*}"
  if [[ "$remainder" == */* ]]; then path="/${remainder#*/}"; else path=""; fi
  scheme="${scheme,,}"
  authority="${authority,,}"
  [[ "$scheme" == http || "$scheme" == https ]] || return 1
  [[ -n "$authority" && "$authority" != *'@'* && "$authority" != *:* ]] || return 1
  while [[ "$path" == */ && "$path" != / ]]; do path="${path%/}"; done
  printf '%s://%s%s\n' "$scheme" "$authority" "$path"
}

apt_source_entry_is_allowed() { # file, URI, suite, component, Signed-By, unsafe trust flags
  local source_file="$1" uri="$2" suite="$3" component="$4" signed_by="$5" unsafe_flags="$6"
  local normalized expected_docker_uri
  case "${unsafe_flags,,}" in
    ""|no|false|0) ;;
    *) return 1 ;;
  esac
  normalized="$(normalize_apt_uri "$uri")" || return 1
  if [[ "$source_file" == "$DOCKER_APT_SOURCE_FILE" ]]; then
    expected_docker_uri="$(normalize_apt_uri "$VELNOR_C1_DOCKER_REPO_URL")" || return 1
    [[ "$normalized" == "$expected_docker_uri" \
      && "$suite" == "$VELNOR_C1_DOCKER_DIST" \
      && "$component" == "$VELNOR_C1_DOCKER_COMPONENT" \
      && "$signed_by" == "$DOCKER_APT_KEY_FILE" ]]
    return
  fi
  [[ "$signed_by" == /usr/share/keyrings/debian-archive-keyring.gpg \
    || "$signed_by" == /usr/share/keyrings/debian-archive-keyring.pgp ]] || return 1
  case "$normalized" in
    http://deb.debian.org/debian|https://deb.debian.org/debian|\
      http://security.debian.org/debian-security|https://security.debian.org/debian-security) ;;
    *) return 1 ;;
  esac
  case "$suite" in
    trixie|trixie-updates|trixie-security) return 0 ;;
    *) return 1 ;;
  esac
}

preflight_active_apt_sources() {
  local source_file uri suite component signed_by unsafe_flags docker_state
  local -a source_files=("$DOCKER_APT_MAIN_SOURCE_FILE" "$DOCKER_APT_SOURCE_DIR"/*.list "$DOCKER_APT_SOURCE_DIR"/*.sources)
  for source_file in "${source_files[@]}"; do
    [[ -e "$source_file" || -L "$source_file" ]] || continue
    [[ ! -L "$source_file" && -f "$source_file" ]] || {
      printf 'APT source is not a regular file: %s\n' "$source_file" >&2
      return 2
    }
    assert_safe_managed_file "$source_file" "APT source file" || return 2
    while IFS=$'\t' read -r uri suite component signed_by unsafe_flags; do
      [[ -n "$uri" && -n "$suite" && -n "$component" ]] || continue
      apt_source_entry_is_allowed "$source_file" "$uri" "$suite" "$component" "$signed_by" "$unsafe_flags" \
        || {
          printf 'APT source is outside the approved Debian/Docker origins or trust policy: %s %s/%s (%s)\n' \
            "$uri" "$suite" "$component" "$source_file" >&2
          return 2
        }
    done < <(apt_source_entries "$source_file")
  done
  if [[ -f "$DOCKER_APT_SOURCE_FILE" ]]; then
    docker_state="$(docker_repo_state "$DOCKER_APT_SOURCE_FILE" \
      "$DOCKER_APT_LEGACY_SOURCE_FILE" "$(docker_apt_repo_line)")" || return 2
    [[ "$docker_state" == current ]] || return 2
    assert_apt_key_readable_by_sandbox "$DOCKER_APT_KEY_FILE" || {
      printf 'active Docker APT source has no safely managed Signed-By key\n' >&2
      return 2
    }
  fi
}

reject_unmanaged_docker_sources() { # managed file, main file, source directory
  local managed="$1" main_file="$2" source_dir="$3" source_file domain
  local -a source_files=("$main_file" "$source_dir"/*.list "$source_dir"/*.sources)
  domain="${VELNOR_C1_DOCKER_REPO_URL#*://}"
  domain="${domain%%/*}"
  for source_file in "${source_files[@]}"; do
    [[ -f "$source_file" && "$source_file" != "$managed" ]] || continue
    if case "$source_file" in
      *.sources)
        awk -v domain="$domain" '
          function finish() {
            if (types ~ /(^|[[:space:]])deb([[:space:]]|$)/ \
                && enabled != "no" && index(tolower(uris), tolower(domain)) > 0) found = 1
          }
          /^[[:space:]]*#/ { next }
          /^[[:space:]]*$/ { finish(); types = ""; uris = ""; enabled = ""; field = ""; next }
          /^[^[:space:]][^:]*:/ {
            field = tolower($0)
            sub(/:.*/, "", field)
            value = $0
            sub(/^[^:]*:[[:space:]]*/, "", value)
            if (field == "types") types = value
            if (field == "uris") uris = value
            if (field == "enabled") enabled = tolower(value)
            next
          }
          /^[[:space:]]+/ {
            value = $0
            sub(/^[[:space:]]+/, "", value)
            if (field == "types") types = types " " value
            if (field == "uris") uris = uris " " value
            if (field == "enabled") enabled = enabled " " tolower(value)
          }
          END { finish(); exit found ? 0 : 1 }
        ' "$source_file"
        ;;
      *.list)
        awk -v domain="$domain" '
          /^[[:space:]]*#/ { next }
          /^[[:space:]]*deb(-src)?[[:space:]]/ {
            line = $0
            sub(/^[[:space:]]*deb(-src)?[[:space:]]+/, "", line)
            sub(/^\[[^]]*\][[:space:]]+/, "", line)
            split(line, fields, /[[:space:]]+/)
            if (tolower(fields[1]) ~ tolower(domain)) found = 1
          }
          END { exit found ? 0 : 1 }
        ' "$source_file"
        ;;
      *) false ;;
    esac; then
      printf 'Docker repository host appears in an unmanaged APT source: %s\n' \
        "$source_file" >&2
      return 2
    fi
  done
}

docker_pin_policy_matches_origin() { # package, exact version -> 0 only for Docker's signed suite
  local pkg="$1" want="$2" policy candidate line version origin
  local in_wanted=0 version_seen=0 source_seen=0 invalid_source=0
  local expected_prefix="${VELNOR_C1_DOCKER_REPO_URL} ${VELNOR_C1_DOCKER_DIST}/${VELNOR_C1_DOCKER_COMPONENT} "
  policy="$(package_read apt_command apt-cache policy "$pkg" 2>/dev/null)" || {
    printf 'cannot read APT candidate policy for %s\n' "$pkg" >&2
    return 2
  }
  candidate="$(awk '$1 == "Candidate:" { print $2; exit }' <<< "$policy")"
  [[ "$candidate" == "$want" ]] || {
    printf 'APT candidate for %s is %s; exact pin is %s\n' \
      "$pkg" "${candidate:-unknown}" "$want" >&2
    return 2
  }
  while IFS= read -r line; do
    if [[ "$line" =~ ^[[:space:]]+(\*\*\*[[:space:]]+)?([^[:space:]]+)[[:space:]]+[0-9]+[[:space:]]*$ ]]; then
      version="${BASH_REMATCH[2]}"
      if [[ "$version" == "$want" ]]; then
        in_wanted=1
        version_seen=1
      else
        in_wanted=0
      fi
      continue
    fi
    if [[ "$in_wanted" == 1 && "$line" =~ ^[[:space:]]+[0-9]+[[:space:]]+(.+)$ ]]; then
      origin="${BASH_REMATCH[1]}"
      if [[ "$origin" == "$expected_prefix"* && "$origin" == *' Packages' ]]; then
        source_seen=1
      else
        invalid_source=1
      fi
    fi
  done <<< "$policy"
  [[ "$version_seen" == 1 && "$source_seen" == 1 && "$invalid_source" == 0 ]] || {
    printf 'APT pin for %s is not exclusively from %s %s/%s via the managed Signed-By source\n' \
      "$pkg" "$VELNOR_C1_DOCKER_REPO_URL" "$VELNOR_C1_DOCKER_DIST" \
      "$VELNOR_C1_DOCKER_COMPONENT" >&2
    return 2
  }
}

verify_docker_package_origins() { # exact package=version specs; call inside exclusive lock
  local spec pkg ver repo_state
  [[ "$PACKAGE_LOCK_HELD" == 1 ]] \
    || die "Docker APT origin verification requires the exclusive package lock"
  preflight_active_apt_sources \
    || die "active APT sources changed to an unapproved origin before package installation" 2
  repo_state="$(docker_repo_state "$DOCKER_APT_SOURCE_FILE" \
    "$DOCKER_APT_LEGACY_SOURCE_FILE" "$(docker_apt_repo_line)")" \
    || die "Docker APT source changed or became unsafe before package installation"
  [[ "$repo_state" == current ]] \
    || die "Docker APT source is not the exact managed Signed-By source before package installation"
  reject_unmanaged_docker_sources "$DOCKER_APT_SOURCE_FILE" \
    "$DOCKER_APT_MAIN_SOURCE_FILE" "$DOCKER_APT_SOURCE_DIR" \
    || die "another active APT source can supply Docker packages"
  assert_apt_key_readable_by_sandbox "$DOCKER_APT_KEY_FILE" \
    || die "Docker APT keyring became unsafe before package installation"
  for spec in "$@"; do
    pkg="${spec%%=*}"
    ver="${spec#*=}"
    docker_pin_policy_matches_origin "$pkg" "$ver" \
      || die "APT candidate origin is unresolved or unsafe for $spec" 2
    log "APT pin origin verified: $spec from $VELNOR_C1_DOCKER_REPO_URL $VELNOR_C1_DOCKER_DIST/$VELNOR_C1_DOCKER_COMPONENT"
  done
}

apt_policy_origin_is_active_source() { # URI, suite, component -> approved active source
  local policy_uri="$1" policy_suite="$2" policy_component="$3"
  local source_file source_uri source_suite source_component signed_by unsafe_flags
  local normalized_policy
  normalized_policy="$(normalize_apt_uri "$policy_uri")" || return 1
  local -a source_files=("$DOCKER_APT_MAIN_SOURCE_FILE" "$DOCKER_APT_SOURCE_DIR"/*.list "$DOCKER_APT_SOURCE_DIR"/*.sources)
  for source_file in "${source_files[@]}"; do
    [[ -f "$source_file" && ! -L "$source_file" ]] || continue
    while IFS=$'\t' read -r source_uri source_suite source_component signed_by unsafe_flags; do
      [[ -n "$source_uri" && "$source_suite" == "$policy_suite" \
        && "$source_component" == "$policy_component" ]] || continue
      [[ "$(normalize_apt_uri "$source_uri")" == "$normalized_policy" ]] || continue
      apt_source_entry_is_allowed "$source_file" "$source_uri" \
        "$source_suite" "$source_component" "$signed_by" "$unsafe_flags" || continue
      return 0
    done < <(apt_source_entries "$source_file")
  done
  return 1
}

verify_apt_candidate_origin() { # package and selected version from apt-get simulation
  local pkg="$1" selected="$2" policy candidate line version origin
  local in_selected=0 version_seen=0 source_seen=0
  policy="$(package_read apt_command apt-cache policy "$pkg" 2>/dev/null)" || {
    printf 'cannot read APT candidate policy for %s\n' "$pkg" >&2
    return 2
  }
  candidate="$(awk '$1 == "Candidate:" { print $2; exit }' <<< "$policy")"
  [[ -n "$candidate" && "$candidate" != "(none)" && "$candidate" == "$selected" ]] || {
    printf 'APT selected version for %s is %s; candidate is %s\n' \
      "$pkg" "${selected:-unknown}" "${candidate:-unknown}" >&2
    return 2
  }
  while IFS= read -r line; do
    if [[ "$line" =~ ^[[:space:]]+(\*\*\*[[:space:]]+)?([^[:space:]]+)[[:space:]]+[0-9]+([[:space:]]+/var/lib/dpkg/status)?[[:space:]]*$ ]]; then
      version="${BASH_REMATCH[2]}"
      if [[ "$version" == "$selected" ]]; then
        in_selected=1
        version_seen=1
      else
        in_selected=0
      fi
      continue
    fi
    [[ "$in_selected" == 1 && "$line" =~ ^[[:space:]]+[0-9]+[[:space:]]+(.+)$ ]] || continue
    origin="${BASH_REMATCH[1]}"
    [[ "$origin" == /var/lib/dpkg/status ]] && continue
    local origin_uri origin_suite_component origin_arch origin_index extra
    read -r origin_uri origin_suite_component origin_arch origin_index extra <<< "$origin"
    [[ -n "$origin_uri" && "$origin_suite_component" == */* \
      && -n "$origin_arch" && "$origin_index" == Packages && -z "$extra" ]] || {
      printf 'unrecognized APT origin for %s %s: %s\n' "$pkg" "$selected" "$origin" >&2
      return 2
    }
    if apt_policy_origin_is_active_source "$origin_uri" \
      "${origin_suite_component%%/*}" "${origin_suite_component#*/}"; then
      source_seen=1
    else
      printf 'APT candidate for %s %s comes from an inactive or unapproved source: %s\n' \
        "$pkg" "$selected" "$origin" >&2
      return 2
    fi
  done <<< "$policy"
  [[ "$version_seen" == 1 && "$source_seen" == 1 ]] || {
    printf 'APT candidate origin is unresolved for %s %s\n' "$pkg" "$selected" >&2
    return 2
  }
  log "APT candidate origin verified: $pkg=$selected"
}

apt_inputs_fingerprint() {
  [[ -n "$SAFE_APT_CONFIG" ]] \
    || die "private APT configuration is missing while fingerprinting package inputs" 2
  python3 "$C1_DIR/apt-input-fingerprint.py" \
    "$SAFE_APT_CONFIG" "$DOCKER_APT_MAIN_SOURCE_FILE" \
    "$DOCKER_APT_SOURCE_DIR" "$DOCKER_APT_KEY_FILE" \
    "${DEBIAN_APT_KEYRINGS[@]}"
}

apt_policy_fingerprint() { # package -> SHA-256 of the exact candidate/origin policy
  local pkg="$1" policy
  policy="$(package_read apt_command apt-cache policy "$pkg" 2>/dev/null)" \
    || die "cannot fingerprint APT policy for $pkg" 2
  printf '%s\n' "$policy" | sha256sum | awk '{ print $1 }'
}

apt_candidate_archive_sha256() { # package, exact version -> authenticated Packages SHA-256
  local pkg="$1" version="$2" metadata hashes
  metadata="$(package_read apt_command apt-cache show "$pkg=$version" 2>/dev/null)" \
    || die "cannot read archive metadata for $pkg=$version" 2
  hashes="$(awk -v wanted_pkg="$pkg" -v wanted_version="$version" '
    BEGIN { RS = ""; FS = "\n" }
    {
      package = version = digest = ""
      for (i = 1; i <= NF; i++) {
        if ($i ~ /^Package:[[:space:]]*/) { package = $i; sub(/^Package:[[:space:]]*/, "", package) }
        if ($i ~ /^Version:[[:space:]]*/) { version = $i; sub(/^Version:[[:space:]]*/, "", version) }
        if ($i ~ /^SHA256:[[:space:]]*/) { digest = $i; sub(/^SHA256:[[:space:]]*/, "", digest) }
      }
      if (package == wanted_pkg && version == wanted_version && digest != "") print digest
    }
  ' <<< "$metadata" | LC_ALL=C sort -u)"
  [[ "$hashes" =~ ^[[:xdigit:]]{64}$ ]] \
    || die "APT metadata does not provide one unambiguous archive SHA-256 for $pkg=$version" 2
  printf '%s\n' "${hashes,,}"
}

apt_version_direction() { # old, new -> APT protocol direction
  local old="$1" new="$2" status=0
  if [[ "$old" == - ]]; then
    printf '<\n'
    return 0
  fi
  if dpkg --compare-versions "$new" gt "$old"; then
    printf '<\n'
    return 0
  else
    status=$?
  fi
  [[ "$status" == 1 ]] || die "cannot compare APT versions $old -> $new" 2
  if dpkg --compare-versions "$new" lt "$old"; then
    printf '>\n'
    return 0
  else
    status=$?
  fi
  [[ "$status" == 1 ]] || die "cannot compare APT versions $old -> $new" 2
  if dpkg --compare-versions "$new" eq "$old"; then
    printf '=\n'
    return 0
  else
    status=$?
  fi
  [[ "$status" == 1 ]] \
    || die "cannot compare APT versions $old -> $new" 2
  die "APT versions are not ordered: $old -> $new" 2
}

verify_install_plan_origins() { # apt-get install options followed by package specs
  local pkg version spec row action raw_pkg base_pkg simulation plan_text plan_file lock_dir keyring
  local old_version direction inputs_fingerprint policy_fingerprint archive_fingerprint evidence_key architecture
  local require_no_installed_upgrades=0
  local -a options=() specs=() simulation_rows=() plan_rows=()
  local -A planned_versions=() planned_old=() planned_direction=()
  local -A plan_action_keys=() seen_package_bases=() policy_hashes=() archive_hashes=()
  [[ "$PACKAGE_LOCK_HELD" == 1 ]] \
    || die "APT resolution requires the exclusive package transaction lock"
  while [[ "$#" -gt 0 && "$1" == --* ]]; do
    if [[ "$1" == --reject-installed-upgrades ]]; then
      require_no_installed_upgrades=1
    else
      options+=("$1")
    fi
    shift
  done
  specs=("$@")
  [[ "${#specs[@]}" -gt 0 ]] || die "APT resolution has no package specs"
  preflight_active_apt_sources \
    || die "active APT sources are outside the approved Debian/Docker repositories" 2
  simulation="$(package_transaction apt_command apt-get --simulate "${options[@]}" install "${specs[@]}")" \
    || die "APT resolver failed before package mutation" 2
  if grep -Eq '^(Remv|Purg)[[:space:]]' <<< "$simulation"; then
    die "APT resolver plans package removals or purges; refusing the transaction" 2
  fi
  plan_text="$(awk '
    $1 == "Inst" || $1 == "Conf" {
      line = $0
      if (!match(line, /\([^)]*\)/)) exit 2
      version_start = RSTART
      selected = substr(line, RSTART + 1, RLENGTH - 2)
      split(selected, fields, /[[:space:]]+/)
      if ($2 == "" || fields[1] == "") exit 2
      if ($1 == "Inst") {
        # The architecture annotation at the end of a new install also uses
        # brackets. Only an old version before the selected-version group
        # means an installed package.
        prefix = substr(line, 1, version_start - 1)
        if (match(prefix, /\[[^]]+\]/)) old = substr(prefix, RSTART + 1, RLENGTH - 2)
        else old = "-"
        printf "UNPACK\t%s\t%s\t%s\n", $2, old, fields[1]
      } else {
        printf "CONFIGURE\t%s\t-\t%s\n", $2, fields[1]
      }
    }
  ' <<< "$simulation")" \
    || die "APT resolver returned a malformed install plan" 2
  if [[ -n "$plan_text" ]]; then mapfile -t simulation_rows <<< "$plan_text"; fi
  [[ "${#simulation_rows[@]}" -gt 0 ]] \
    || die "APT resolver produced no install actions for missing packages: ${specs[*]}" 2
  for row in "${simulation_rows[@]}"; do
    IFS=$'\t' read -r action raw_pkg old_version version <<< "$row"
    [[ -n "$raw_pkg" && -n "$version" ]] \
      || die "APT resolver returned a malformed install action" 2
    base_pkg="${raw_pkg%%:*}"
    if [[ "$action" == UNPACK ]]; then
      [[ -z "${seen_package_bases[$base_pkg]+present}" ]] \
        || die "APT resolver returned an architecture-ambiguous transaction for $base_pkg" 2
      seen_package_bases["$base_pkg"]=1
      direction="$(apt_version_direction "$old_version" "$version")"
      if [[ "$old_version" != - && "$direction" == '>' ]]; then
        die "APT resolver would downgrade installed package $base_pkg from $old_version to $version" 2
      fi
      if [[ "$require_no_installed_upgrades" == 1 \
        && "$old_version" != - && "$direction" != = ]]; then
        die "installing missing packages would change installed dependency $base_pkg ($old_version -> $version)" 2
      fi
      planned_versions["$raw_pkg"]="$version"
      planned_versions["$base_pkg"]="$version"
      planned_old["$base_pkg"]="$old_version"
      planned_direction["$base_pkg"]="$direction"
      plan_rows+=("ACTION"$'\t'"UNPACK"$'\t'"$base_pkg"$'\t'"$old_version"$'\t'"$direction"$'\t'"$version")
    else
      [[ "$action" == CONFIGURE \
        && "${planned_versions[$raw_pkg]:-${planned_versions[$base_pkg]:-}}" == "$version" ]] \
        || die "APT resolver would configure unplanned package $raw_pkg=$version" 2
      old_version="${planned_old[$base_pkg]:-}"
      direction="${planned_direction[$base_pkg]:-}"
      [[ -n "$old_version" && -n "$direction" ]] \
        || die "APT resolver configure action lacks an unpack plan for $base_pkg" 2
      plan_rows+=("ACTION"$'\t'"CONFIGURE"$'\t'"$base_pkg"$'\t'"$old_version"$'\t'"$direction"$'\t'"$version")
    fi
    local plan_key="$action"$'\t'"$base_pkg"$'\t'"$old_version"$'\t'"$direction"$'\t'"$version"
    [[ -z "${plan_action_keys[$plan_key]+present}" ]] \
      || die "APT resolver returned a duplicate package action for $base_pkg" 2
    plan_action_keys["$plan_key"]=1
  done
  for spec in "${specs[@]}"; do
    pkg="${spec%%=*}"
    version="${spec#*=}"
    [[ -n "${planned_versions[$pkg]:-${planned_versions[${pkg%%:*}]:-}}" ]] \
      || die "APT resolver omitted requested package $pkg" 2
    if [[ "$spec" == *=* && "${planned_versions[$pkg]:-${planned_versions[${pkg%%:*}]:-}}" != "$version" ]]; then
      die "APT resolver selected ${planned_versions[$pkg]:-${planned_versions[${pkg%%:*}]:-}} for $pkg; exact requested version is $version" 2
    fi
  done
  for row in "${plan_rows[@]}"; do
    IFS=$'\t' read -r _ action pkg old_version direction version <<< "$row"
    [[ "$action" == UNPACK ]] || continue
    verify_apt_candidate_origin "$pkg" "$version" \
      || die "APT candidate origin is unresolved or unsafe for $pkg=$version" 2
    policy_fingerprint="$(apt_policy_fingerprint "$pkg")"
    archive_fingerprint="$(apt_candidate_archive_sha256 "$pkg" "$version")"
    policy_hashes["$pkg|$version"]="$policy_fingerprint"
    archive_hashes["$pkg|$version"]="$archive_fingerprint"
  done
  preflight_active_apt_sources \
    || die "active APT sources changed to an unapproved origin before package installation" 2
  inputs_fingerprint="$(apt_inputs_fingerprint)" \
    || die "cannot fingerprint APT config, source, and key inputs" 2
  architecture="$(maintenance_command dpkg --print-architecture)" \
    || die "cannot resolve native APT architecture for the transaction plan" 2
  [[ "$architecture" =~ ^[a-z0-9][a-z0-9-]*$ ]] \
    || die "native APT architecture is malformed: $architecture" 2
  lock_dir="${PACKAGE_LOCK_PATH%/*}"
  plan_file="$(mktemp "$lock_dir/c1-apt-plan.XXXXXX")" \
    || die "cannot create transaction-evidence APT install plan" 2
  track_temp "$plan_file"
  {
    printf 'META\tINPUTS\t%s\t%s\t%s\t%s\t%s\n' \
      "$SAFE_APT_CONFIG" "$DOCKER_APT_MAIN_SOURCE_FILE" \
      "$DOCKER_APT_SOURCE_DIR" "$inputs_fingerprint" "$architecture"
    for keyring in "$DOCKER_APT_KEY_FILE" "${DEBIAN_APT_KEYRINGS[@]}"; do
      printf 'META\tKEYRING\t%s\n' "$keyring"
    done
    while IFS='|' read -r pkg version; do
      [[ -n "$pkg" && -n "$version" ]] || continue
      evidence_key="$pkg|$version"
      printf 'META\tPOLICY\t%s\t%s\t%s\n' \
        "$pkg" "$version" "${policy_hashes["$evidence_key"]}"
      printf 'META\tARCHIVE\t%s\t%s\t%s\n' \
        "$pkg" "$version" "${archive_hashes["$evidence_key"]}"
    done < <(printf '%s\n' "${!policy_hashes[@]}" | LC_ALL=C sort)
    printf '%s\n' "${plan_rows[@]}" | LC_ALL=C sort
  } > "$plan_file" \
    || die "cannot write APT transaction evidence plan" 2
  chmod 0600 "$plan_file" && chown 0:0 "$plan_file" \
    || die "cannot secure transaction-bound APT install plan" 2
  assert_safe_managed_file "$plan_file" "APT install plan" \
    || die "APT transaction evidence plan has unsafe metadata" 2
  APPROVED_APT_PLAN_FILE="$plan_file"
}

apt_plan_guarded_install() {
  [[ "$PACKAGE_LOCK_HELD" == 1 && -n "$APPROVED_APT_PLAN_FILE" ]] \
    || die "APT install lacks an exclusive lock or reviewed resolver plan" 2
  local hook="$C1_DIR/apt-plan-guard.sh"
  [[ -x "$hook" && -f "$hook" && ! -L "$hook" ]] \
    || die "APT transaction plan guard is missing or unsafe" 2
  apt_command apt-get \
    -o "DPkg::Pre-Install-Pkgs::=$hook $APPROVED_APT_PLAN_FILE" \
    -o "DPkg::Tools::Options::$hook::Version=2" \
    "$@"
}

write_new_apt_repo() { # exact content, path; atomically create without clobbering
  local want="$1" repo_file="$2" tmp
  assert_safe_managed_directory "$(dirname -- "$repo_file")" "APT source directory" \
    || die "unsafe APT source directory"
  [[ ! -e "$repo_file" && ! -L "$repo_file" ]] \
    || die "Docker APT source appeared during setup; refusing to replace it"
  tmp="$(mktemp "${repo_file}.XXXXXX")" \
    || die "cannot create a temporary Docker APT source file"
  track_temp "$tmp"
  printf '%s\n' "$want" > "$tmp"
  chmod 0644 "$tmp"
  chown 0:0 "$tmp"
  [[ ! -e "$repo_file" && ! -L "$repo_file" ]] \
    || die "Docker APT source appeared during setup; refusing to replace it"
  ln -T -- "$tmp" "$repo_file" \
    || die "cannot atomically create Docker APT source without replacing an existing file"
  rm -f -- "$tmp"
}

step_docker_packages() { # C3 pinned + drain gate
  log "step: pinned docker set (C3)"
  local specs=(
    "docker-ce=$VELNOR_C1_DOCKER_CE_VERSION"
    "docker-ce-cli=$VELNOR_C1_DOCKER_CE_CLI_VERSION"
    "containerd.io=$VELNOR_C1_CONTAINERD_VERSION"
    "docker-buildx-plugin=$VELNOR_C1_BUILDX_VERSION"
    "docker-compose-plugin=$VELNOR_C1_COMPOSE_VERSION"
  )
  local todo=() s pkg ver
  for s in "${specs[@]}"; do
    pkg="${s%%=*}"; ver="${s#*=}"
    if ! pkg_installed "$pkg" "$ver"; then
      assert_installed_package_not_newer_than_pin "$pkg" "$ver"
      todo+=("$s")
    fi
  done
  if [[ "${#todo[@]}" -eq 0 ]]; then
    log "docker set already at pinned versions"
    return 0
  fi
  log "to install: ${todo[*]}"
  if [[ "$CHECK" == 1 ]]; then
    die "APT resolution unresolved in --check for exact Docker pins: ${todo[*]}; no fresh index or resolver simulation was run" 2
  fi
  ensure_maintenance_barrier "install ${todo[*]} (restarts dockerd)"
  require_drained "install ${todo[*]} before APT list refresh"
  apt_update_once
  require_drained "install ${todo[*]} after APT list refresh"
  verify_docker_package_origins "${todo[@]}"
  verify_install_plan_origins --no-remove "${todo[@]}"
  require_drained "install ${todo[*]} immediately before package transaction"
  preflight_active_apt_sources \
    || die "active APT sources changed after Docker resolver review" 2
  # Exact pins, no --allow-downgrades: older installs converge upward; a
  # newer-than-pin install fails closed. The approved full plan blocks unrelated
  # resolver actions, and --no-remove blocks package removals.
  # Previously held Docker packages are unheld, installed, and re-held while
  # the main process retains the exclusive package transaction lock.
  local install_script
  install_script="$(cat <<'SCRIPT'
set -euo pipefail
hook="$1"
plan_file="$2"
shift 2
specs=("$@")
held=()
transaction_started=0
restore_package_holds() {
  rc=$?
  trap - EXIT
  set +e
  if [[ "$transaction_started" != 1 ]]; then exit "$rc"; fi
  for spec in "${specs[@]}"; do
    pkg="${spec%%=*}"
    status="$(dpkg-query -W -f='${Status}' "$pkg" 2>/dev/null)"
    keep=0
    for old_hold in "${held[@]}"; do
      [[ "$old_hold" == "$pkg" ]] && keep=1
    done
    case "$status" in
      "install ok installed"|"hold ok installed"|"install ok unpacked"|"install ok half-configured"|"install ok half-installed") keep=1 ;;
    esac
    if [[ "$keep" == 1 ]]; then apt-mark hold "$pkg" || rc=1; fi
  done
  exit "$rc"
}
trap restore_package_holds EXIT
holds="$(apt-mark showhold)" || {
  printf 'cannot read APT holds; refusing Docker package transaction\n' >&2
  exit 2
}
for spec in "${specs[@]}"; do
  pkg="${spec%%=*}"
  if printf '%s\n' "$holds" | grep -qx "$pkg"; then
    held+=("$pkg")
    transaction_started=1
    apt-mark unhold "$pkg"
  fi
done
transaction_started=1
apt-get \
  -o "DPkg::Pre-Install-Pkgs::=$hook $plan_file" \
  -o "DPkg::Tools::Options::$hook::Version=2" \
  install -y --no-remove "${specs[@]}"
for spec in "${specs[@]}"; do apt-mark hold "${spec%%=*}"; done
trap - EXIT
SCRIPT
)"
  DOCKER_CHANGE_STARTED=1
  DOCKER_LOCAL_HEALTH_VERIFIED=0
  ensure_safe_apt_config
  package_transaction bash -euo pipefail -c "$install_script" c1-docker-install \
    "$C1_DIR/apt-plan-guard.sh" "$APPROVED_APT_PLAN_FILE" "${todo[@]}"
}

step_holds() { # drain+holds semantics: nothing may float after this
  log "step: apt holds (docker set + velnor-runner if present)"
  local pkgs=(docker-ce docker-ce-cli containerd.io docker-buildx-plugin docker-compose-plugin)
  if pkg_installed velnor-runner; then pkgs+=(velnor-runner); fi
  local held
  if ! held="$(package_read apt_command apt-mark showhold 2>/dev/null)"; then
    if [[ "$CHECK" == 1 ]]; then
      die "APT hold state is unresolved: cannot read current package holds" 2
    fi
    die "cannot read APT holds; refusing to change package holds"
  fi
  local todo=() p
  for p in "${pkgs[@]}"; do
    printf '%s\n' "$held" | grep -qx "$p" || todo+=("$p")
  done
  if [[ "${#todo[@]}" -eq 0 ]]; then
    log "holds already in place: ${pkgs[*]}"
  else
    if [[ "$CHECK" == 0 ]]; then
      ensure_maintenance_barrier "reconcile APT package holds"
      held="$(package_read apt_command apt-mark showhold 2>/dev/null)" \
        || die "cannot reread APT holds inside package transaction"
      todo=()
      for p in "${pkgs[@]}"; do
        printf '%s\n' "$held" | grep -qx "$p" || todo+=("$p")
      done
    fi
    if [[ "${#todo[@]}" -gt 0 ]]; then
      package_transaction apt_command apt-mark hold "${todo[@]}"
    fi
  fi
  log "runner upgrades must unhold and re-hold velnor-runner inside the same exclusive package-lock transaction"
}

daemon_want() {
  if [[ "$WANT_POOLS" == "1" ]]; then
    printf '{\n  "log-opts": {\n    "max-size": "10m"\n  },\n  "default-address-pools": [\n    {\n      "base": "172.30.0.0/16",\n      "size": 24\n    }\n  ]\n}\n'
  else
    printf '{\n  "log-opts": {\n    "max-size": "10m"\n  }\n}\n'
  fi
}

daemon_config_state() { # path, exact expected content -> current|different|missing
  local path="$1" want="$2"
  if [[ ! -e "$path" && ! -L "$path" ]]; then
    printf 'missing\n'
    return 0
  fi
  assert_safe_managed_file "$path" "Docker daemon config" || return 2
  if cmp -s -- "$path" <(printf '%s\n' "$want"); then
    printf 'current\n'
  else
    printf 'different\n'
  fi
}

step_daemon_json() { # C5: merge managed keys while preserving every other setting
  log "step: daemon.json (C5)"
  if [[ "$WANT_POOLS" == "1" ]]; then
    [[ "$POOL_CONFLICT" -eq 0 ]] \
      || die "VELNOR_C1_DOCKER_POOLS=1 but host routes conflict; refusing pools (spec §6: never alter the public route for container networks)"
    log "pools requested and route-clear: including 172.30.0.0/16"
  else
    log "pools not requested: selene-style omit (log limits only)"
  fi
  local path=/etc/docker/daemon.json config_dir=/etc/docker want tmp mode owner before_sha current_sha docker_was_active=0 config_state
  if systemd_available && systemctl is-active -q docker 2>/dev/null; then
    docker_was_active=1
  fi
  if [[ -e "$path" || -L "$path" ]]; then
    [[ ! -L "$path" && -f "$path" ]] \
      || die "daemon.json must be a regular file; refusing to follow or replace a special file"
  fi
  if command -v python3 >/dev/null 2>&1; then
    want="$(python3 "$C1_DIR/merge-daemon-json.py" "$path" "$WANT_POOLS")" \
      || die "cannot safely merge daemon.json; existing settings are unchanged"
  elif [[ "$CHECK" -eq 1 && ! -e "$path" && ! -L "$path" ]]; then
    want="$(daemon_want)"
  else
    die "python3 is required to preserve daemon.json settings"
  fi
  if [[ -e "$config_dir" || -L "$config_dir" ]]; then
    assert_safe_managed_directory "$config_dir" "Docker config directory" \
      || die "unsafe Docker config directory"
  fi
  config_state="$(daemon_config_state "$path" "$want")" \
    || die "unsafe daemon.json ownership or mode"
  if [[ "$config_state" == current ]]; then
    log "daemon.json already current"
    return 0
  fi
  if [[ "$CHECK" -eq 1 ]]; then
    PENDING=$((PENDING + 1))
    printf '    would: atomically merge managed keys into %s:\n%s\n' "$path" "$want"
  else
    ensure_maintenance_barrier "rewrite daemon.json"
    [[ -d "$config_dir" ]] || mut install -m 0755 -d "$config_dir"
    mode=0644
    owner=0:0
    before_sha=missing
    if [[ -f "$path" ]]; then
      [[ "$(stat -c '%h' -- "$path")" == 1 ]] \
        || die "daemon.json has multiple hard links; refusing replacement"
      mode="$(stat -c '%a' -- "$path")" || die "cannot read daemon.json mode"
      owner="$(stat -c '%u:%g' -- "$path")" || die "cannot read daemon.json owner"
      before_sha="$(sha256sum -- "$path" | awk '{print $1}')" || die "cannot hash daemon.json"
    fi
    tmp="$(mktemp "$config_dir/.daemon.json.XXXXXX")"
    track_temp "$tmp"
    if command -v python3 >/dev/null 2>&1; then
      python3 "$C1_DIR/merge-daemon-json.py" "$path" "$WANT_POOLS" > "$tmp" \
        || die "cannot safely render daemon.json; existing settings are unchanged"
    else
      printf '%s\n' "$want" > "$tmp"
    fi
    chmod "$mode" "$tmp"
    chown "$owner" "$tmp"
    if [[ "$before_sha" != missing ]]; then
      current_sha="$(sha256sum -- "$path" | awk '{print $1}')" || die "cannot recheck daemon.json"
      [[ "$current_sha" == "$before_sha" ]] \
        || die "daemon.json changed during merge; refusing to replace the newer file"
    elif [[ -e "$path" || -L "$path" ]]; then
      die "daemon.json appeared during merge; refusing to replace it"
    fi
    mv -f -- "$tmp" "$path"
    log "atomically updated $path while preserving unmanaged settings"
  fi
  if [[ "$docker_was_active" -eq 1 ]]; then
    restart_docker
  fi
}

restart_docker() {
  ensure_maintenance_barrier "restart Docker"
  [[ "$PACKAGE_LOCK_HELD" == 1 ]] \
    || die "Docker restart lacks the exclusive maintenance barrier"
  # Fence any admission unit that was reactivated after the original drain.
  # The exclusive package lock blocks its shared ExecStart lock while units
  # stop; the final inventory and restart remain inside the same barrier.
  close_velnor_admission
  wait_for_work_drain
  require_drained "restart Docker"
  if [[ "$CHECK" == 0 ]]; then
    DOCKER_CHANGE_STARTED=1
    DOCKER_LOCAL_HEALTH_VERIFIED=0
  fi
  mut systemctl restart docker
}

step_service() { # C4
  log "step: docker service enabled+started (C4)"
  preflight_systemd
  local needs_change=0
  if ! systemctl is-enabled -q docker 2>/dev/null; then needs_change=1; fi
  if ! systemctl is-active -q docker 2>/dev/null; then needs_change=1; fi
  if [[ "$CHECK" == 0 && "$needs_change" == 1 ]]; then
    ensure_maintenance_barrier "enable or start Docker"
  fi
  if ! systemctl is-enabled -q docker 2>/dev/null; then mut systemctl enable docker; fi
  if ! systemctl is-active -q docker 2>/dev/null; then
    if [[ "$CHECK" == 0 ]]; then
      close_velnor_admission
      wait_for_work_drain
      require_drained "start Docker"
      docker_runtime_inactive \
        || die "Docker became active or its local runtime state changed before start; refusing an unverified start" 2
    fi
    if [[ "$CHECK" == 0 ]]; then
      DOCKER_CHANGE_STARTED=1
      DOCKER_LOCAL_HEALTH_VERIFIED=0
    fi
    mut systemctl start docker
  else
    log "docker already active"
  fi
}

# -------------------------------------------------------------- post-checks
# Verify-only. Divergence is reported in check mode; unknown Docker health is fatal in both modes.

post_checks() {
  log "post-checks (verify-only)"
  DOCKER_LOCAL_HEALTH_VERIFIED=0
  local fail=0
  local specs=(
    "docker-ce=$VELNOR_C1_DOCKER_CE_VERSION"
    "docker-ce-cli=$VELNOR_C1_DOCKER_CE_CLI_VERSION"
    "containerd.io=$VELNOR_C1_CONTAINERD_VERSION"
    "docker-buildx-plugin=$VELNOR_C1_BUILDX_VERSION"
    "docker-compose-plugin=$VELNOR_C1_COMPOSE_VERSION"
  )
  local s pkg ver
  for s in "${specs[@]}"; do
    pkg="${s%%=*}"; ver="${s#*=}"
    if pkg_installed "$pkg" "$ver"; then
      log "pin OK: $s"
    else
      warn "pin MISMATCH: want $s, have $(package_read dpkg-query -W -f='${Version}' "$pkg" 2>/dev/null || echo none)"
      fail=1
    fi
  done
  local held
  if ! held="$(package_read apt_command apt-mark showhold 2>/dev/null)"; then
    if [[ "$CHECK" == 1 ]]; then
      die "APT hold state is unresolved: cannot verify package holds" 2
    fi
    die "cannot verify APT package holds"
  fi
  for pkg in docker-ce docker-ce-cli containerd.io docker-buildx-plugin docker-compose-plugin; do
    if printf '%s\n' "$held" | grep -qx "$pkg"; then
      log "hold OK: $pkg"
    else
      warn "hold MISSING: $pkg"
      fail=1
    fi
  done
  if [[ -f /etc/docker/daemon.json ]]; then
    assert_safe_managed_file /etc/docker/daemon.json "Docker daemon config" \
      || die "daemon.json ownership or mode became unsafe"
    if command -v python3 >/dev/null 2>&1; then
      if python3 -c 'import json; json.load(open("/etc/docker/daemon.json"))'; then
        log "daemon.json valid JSON"
      else
        warn "daemon.json INVALID"
        fail=1
      fi
    else
      log "daemon.json present (no python3 to validate)"
    fi
  else
    warn "daemon.json MISSING"
    fail=1
  fi
  local snapshot root storage logging driver cgroup
  snapshot="$(require_docker_info_snapshot)"
  IFS='|' read -r root storage logging driver cgroup <<< "$snapshot"
  if [[ "$driver" == "systemd" ]]; then
    log "cgroup driver: systemd"
  else
    warn "cgroup driver: $driver"
    fail=1
  fi
  if [[ "$cgroup" == "2" ]]; then
    log "cgroup version: 2"
  else
    warn "cgroup version: $cgroup"
    fail=1
  fi
  if [[ "$driver" == systemd && "$cgroup" == "2" ]]; then
    DOCKER_LOCAL_HEALTH_VERIFIED=1
  fi
  if host_has_libvirt_or_qemu; then
    warn "libvirt/KVM/QEMU present — violates spec §6"
    fail=1
  else
    log "no libvirt/KVM/QEMU"
  fi
  if [[ "$fail" -ne 0 ]]; then
    if [[ "$CHECK" -eq 1 ]]; then
      warn "post-checks report divergence (expected before first apply)"
    else
      die "post-checks FAILED"
    fi
  else
    log "post-checks all green"
  fi
}

# -------------------------------------------------------------------- main

main() {
  case "${1:-}" in
    --help|-h) usage; return 0 ;;
    --check) CHECK=1; log "CHECK mode: no persistent host changes; unknown Docker/APT state fails closed" ;;
    "") : ;;
    *) die "unknown argument: $1 (see --help)" ;;
  esac
  [[ "$#" -le 1 ]] || die "unexpected extra argument: ${*:2} (see --help)"
  [[ "$(id -u)" -eq 0 ]] || die "must run as root (target user: ${VELNOR_C1_TARGET_USER})"
  [[ "$WANT_POOLS" == 0 || "$WANT_POOLS" == 1 ]] \
    || die "VELNOR_C1_DOCKER_POOLS must be 0 or 1"
  [[ "$ALLOW_RESTART" == 0 || "$ALLOW_RESTART" == 1 ]] \
    || die "VELNOR_C1_ALLOW_RESTART must be 0 or 1"
  [[ "$MAINTENANCE_TIMEOUT" =~ ^[1-9][0-9]*$ ]] \
    || die "VELNOR_C1_DRAIN_TIMEOUT_SECONDS must be a positive integer"
  trap cleanup EXIT
  MAINTENANCE_DEADLINE=$((SECONDS + MAINTENANCE_TIMEOUT))
  acquire_promotion_shared_lock
  ensure_safe_apt_config
  log "C1 host-setup target: ${VELNOR_C1_TARGET_NAME} (${VELNOR_C1_TARGET_USER}@${VELNOR_C1_TARGET_HOST})"
  preflight_os
  preflight_routes
  preflight_work
  preflight_env
  preflight_host_invariants
  step_timezone
  step_base_packages
  step_git_lfs_bat
  step_docker_key
  step_docker_repo
  step_docker_packages
  step_holds
  step_permit_ledger_roster
  step_daemon_json
  step_service
  post_checks
  if [[ "$CHECK" -eq 1 ]]; then
    [[ "$WORK_INVENTORY_UNKNOWN" -eq 0 ]] \
      || die "CHECK incomplete: Docker work inventory could not be verified" 2
    if [[ "$PENDING" -gt 0 ]]; then
      log "CHECK result: $PENDING change(s) pending"
      exit 1
    fi
    log "CHECK result: converged, no changes pending"
  else
    restore_admission_units \
      || die "Docker setup converged but previously active Velnor units could not be restarted after releasing the package lock"
    log "DONE: C1 host-setup converged"
  fi
}

if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
  main "$@"
fi
