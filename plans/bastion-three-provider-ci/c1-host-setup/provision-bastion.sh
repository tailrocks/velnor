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
#   provision-bastion.sh [--check]     # --check: read-only, exit 1 if changes pending
#   provision-bastion.sh --help
#
# Environment:
#   VELNOR_C1_DOCKER_POOLS=1   opt-in 172.30.0.0/16 daemon pools (default: off,
#                              selene-style omit; refused if routes conflict)
#   VELNOR_C1_ALLOW_RESTART=1  confirm dockerd restart with running containers
#                              (set only after draining jobs; never by default)
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

C1_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck disable=SC1091 # resolved relative to C1_DIR at runtime
. "$C1_DIR/pins.env"
# shellcheck disable=SC1091
. "$C1_DIR/targets.env"

CHECK=0
PENDING=0
ALLOW_RESTART="${VELNOR_C1_ALLOW_RESTART:-0}"
WANT_POOLS="${VELNOR_C1_DOCKER_POOLS:-0}"
CLEANUP_PATHS=()

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
  "$@"
}

usage() {
  sed -n '2,/^set -euo/p' "${BASH_SOURCE[0]}" | sed 's/^# \?//'
}

cleanup() {
  local path
  for path in "${CLEANUP_PATHS[@]}"; do rm -f -- "$path"; done
}

track_temp() {
  CLEANUP_PATHS+=("$1")
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
  if command -v docker >/dev/null 2>&1; then
    local ids line count=0
    ids="$(docker ps -q 2>/dev/null)" || return 2
    while IFS= read -r line; do
      [[ -n "$line" ]] || continue
      count=$((count + 1))
    done <<< "$ids"
    printf '%s\n' "$count"
  else
    echo 0
  fi
}

preflight_work() {
  log "pre-flight: running-work read"
  local n
  if ! n="$(running_containers)"; then
    warn "running-container inventory is unknown: docker ps failed"
    WORK_INVENTORY_UNKNOWN=1
    [[ "$CHECK" -eq 1 ]] \
      || die "refusing apply because Docker work inventory cannot be proven" 2
    return 0
  fi
  log "running containers: $n"
  if [[ "$n" -gt 0 ]]; then
    docker ps --format '{{.ID}} {{.Image}} {{.Status}}' 2>/dev/null || true
  fi
  if [[ -d /run/systemd/system ]] && command -v systemctl >/dev/null 2>&1; then
    local units; units="$(systemctl list-units --type=service --state=running --plain --no-legend 'velnor*' 2>/dev/null || true)"
    if [[ -n "$units" ]]; then log "active velnor units:"; printf '%s\n' "$units"; fi
    local sockets; sockets="$(systemctl list-sockets --all --no-legend --plain 2>/dev/null || true)"
    if [[ -n "$sockets" ]]; then log "systemd sockets:"; printf '%s\n' "$sockets"; fi
  fi
}

preflight_docker_config() {
  local path=/etc/docker/daemon.json
  log "pre-flight: existing Docker daemon config (read-only)"
  if [[ ! -e "$path" && ! -L "$path" ]]; then
    log "daemon.json absent"
    return 0
  fi
  command -v python3 >/dev/null 2>&1 \
    || die "python3 is required to inspect existing daemon.json safely; refusing before host changes"
  [[ ! -L "$path" && -f "$path" ]] \
    || die "daemon.json must be a regular file, not a symlink or special file"
  [[ "$(stat -c '%h' -- "$path")" == 1 ]] \
    || die "daemon.json has multiple hard links; refusing replacement"
  python3 "$C1_DIR/merge-daemon-json.py" "$path" inspect \
    || die "cannot inspect existing daemon.json safely"
  python3 "$C1_DIR/merge-daemon-json.py" "$path" "$WANT_POOLS" >/dev/null \
    || die "existing daemon.json cannot be safely merged; review it before host changes"
  log "daemon.json is valid and preserves all unmanaged settings"
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
  if pkg_installed velnor-runner; then
    [[ -f /run/velnor/package-transaction.lock && ! -L /run/velnor/package-transaction.lock ]] \
      || die "velnor-runner is installed but its package transaction lock is missing or unsafe"
    log "Velnor package transaction lock is present"
  fi
  if command -v docker >/dev/null 2>&1 && docker info >/dev/null 2>&1; then
    docker info --format 'Docker: root={{.DockerRootDir}} storage={{.Driver}} logging={{.LoggingDriver}} cgroup={{.CgroupDriver}}/{{.CgroupVersion}}' 2>/dev/null || true
  else
    log "Docker daemon not reachable yet; runtime config will be checked after setup"
  fi
}

# Drain gate: refuse to disturb running work unless the operator explicitly
# confirms (after draining) via VELNOR_C1_ALLOW_RESTART=1. Re-reads live.
require_drained() {
  local reason="$1"
  local n
  n="$(running_containers)" \
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
  local pkg="$1" want="${2:-}" status ver
  status="$(dpkg-query -W -f='${Status}' "$pkg" 2>/dev/null || echo "not-installed")"
  # NB: held packages report "hold ok installed" — accept both desired states.
  [[ "$status" == "install ok installed" || "$status" == "hold ok installed" ]] || return 1
  if [[ -n "$want" ]]; then
    ver="$(dpkg-query -W -f='${Version}' "$pkg" 2>/dev/null || echo "")"
    [[ "$ver" == "$want" ]] || return 1
  fi
  return 0
}

APT_UPDATED=0
apt_update_once() {
  if [[ "$APT_UPDATED" -eq 0 ]]; then
    mut apt-get update
    APT_UPDATED=1
  fi
}

# ensure_present_pkgs: `state: present` semantics — install ONLY missing
# packages, never upgrade anything already installed.
ensure_present_pkgs() {
  local missing=() p
  for p in "$@"; do
    pkg_installed "$p" || missing+=("$p")
  done
  if [[ "${#missing[@]}" -eq 0 ]]; then
    log "all present: $*"
    return 0
  fi
  apt_update_once
  mut apt-get install -y --no-upgrade "${missing[@]}"
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
  [[ ! -L /etc/apt/keyrings/docker.asc && (! -e /etc/apt/keyrings/docker.asc || -f /etc/apt/keyrings/docker.asc) ]] \
    || die "existing Docker keyring path is a symlink or special file; refusing to follow it"
  if [[ -f /etc/apt/keyrings/docker.asc ]] && cmp -s "$tmp" /etc/apt/keyrings/docker.asc; then
    log "keyring already current"
    return 0
  fi
  if [[ -f /etc/apt/keyrings/docker.asc ]]; then
    die "installed docker key differs from verified download; refusing silent rotation (remove /etc/apt/keyrings/docker.asc manually after review, then re-run)"
  fi
  mut install -m 0755 -d /etc/apt/keyrings
  mut install -m 0644 "$tmp" /etc/apt/keyrings/docker.asc
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
    mut write_new_apt_repo "$want" "$repo_file"
    APT_UPDATED=0 # repo changed -> refresh before installs
  fi
}

docker_repo_state() { # path, legacy path, exact expected content -> current|missing
  local repo_file="$1" legacy_file="$2" want="$3"
  if [[ -e "$legacy_file" || -L "$legacy_file" ]]; then
    printf 'legacy Docker APT source exists: %s\n' "$legacy_file" >&2
    return 2
  fi
  if [[ ! -e "$repo_file" && ! -L "$repo_file" ]]; then
    printf 'missing\n'
    return 0
  fi
  if [[ -L "$repo_file" || ! -f "$repo_file" ]]; then
    printf 'Docker APT source is not a regular file: %s\n' "$repo_file" >&2
    return 2
  fi
  if [[ "$(stat -c '%h' -- "$repo_file" 2>/dev/null)" != 1 ]]; then
    printf 'Docker APT source has multiple hard links: %s\n' "$repo_file" >&2
    return 2
  fi
  if cmp -s <(printf '%s\n' "$want") "$repo_file"; then
    printf 'current\n'
    return 0
  fi
  printf 'Docker APT source has unrecognized content: %s\n' "$repo_file" >&2
  return 2
}

write_new_apt_repo() { # exact content, path; atomically create without clobbering
  local want="$1" repo_file="$2" tmp
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
    if ! pkg_installed "$pkg" "$ver"; then todo+=("$s"); fi
  done
  if [[ "${#todo[@]}" -eq 0 ]]; then
    log "docker set already at pinned versions"
    return 0
  fi
  log "to install: ${todo[*]}"
  require_drained "install ${todo[*]} (restarts dockerd)"
  apt_update_once
  # Exact pins, no --allow-downgrades: a newer-than-pin install fails closed.
  # Previously held Docker packages are unheld, upgraded, and held again inside
  # one transaction. If Velnor is installed, the shell itself owns its lock so
  # package changes cannot race active Velnor services.
  local install_script
  install_script="$(cat <<'SCRIPT'
set -euo pipefail
specs=("$@")
held=()
restore_package_holds() {
  rc=$?
  trap - EXIT
  set +e
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
for spec in "${specs[@]}"; do
  pkg="${spec%%=*}"
  if apt-mark showhold | grep -qx "$pkg"; then
    held+=("$pkg")
    apt-mark unhold "$pkg"
  fi
done
apt-get install -y "${specs[@]}"
for spec in "${specs[@]}"; do apt-mark hold "${spec%%=*}"; done
trap - EXIT
SCRIPT
)"
  if pkg_installed velnor-runner; then
    mut /usr/bin/flock --exclusive --nonblock --no-fork \
      /run/velnor/package-transaction.lock \
      bash -euo pipefail -c "$install_script" c1-docker-install "${todo[@]}"
  else
    mut bash -euo pipefail -c "$install_script" c1-docker-install "${todo[@]}"
  fi
}

step_holds() { # drain+holds semantics: nothing may float after this
  log "step: apt holds (docker set + velnor-runner if present)"
  local pkgs=(docker-ce docker-ce-cli containerd.io docker-buildx-plugin docker-compose-plugin)
  if pkg_installed velnor-runner; then pkgs+=(velnor-runner); fi
  local held; held="$(apt-mark showhold 2>/dev/null || true)"
  local todo=() p
  for p in "${pkgs[@]}"; do
    printf '%s\n' "$held" | grep -qx "$p" || todo+=("$p")
  done
  if [[ "${#todo[@]}" -eq 0 ]]; then
    log "holds already in place: ${pkgs[*]}"
  else
    mut apt-mark hold "${todo[@]}"
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

step_daemon_json() { # C5: merge managed keys while preserving every other setting
  log "step: daemon.json (C5)"
  if [[ "$WANT_POOLS" == "1" ]]; then
    [[ "$POOL_CONFLICT" -eq 0 ]] \
      || die "VELNOR_C1_DOCKER_POOLS=1 but host routes conflict; refusing pools (spec §6: never alter the public route for container networks)"
    log "pools requested and route-clear: including 172.30.0.0/16"
  else
    log "pools not requested: selene-style omit (log limits only)"
  fi
  local path=/etc/docker/daemon.json config_dir=/etc/docker want tmp mode owner before_sha current_sha
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
  if [[ -f "$path" ]] && cmp -s -- "$path" <(printf '%s\n' "$want"); then
    log "daemon.json already current"
    return 0
  fi
  local active=0
  if [[ -d /run/systemd/system ]] && command -v systemctl >/dev/null 2>&1 \
     && systemctl is-active -q docker 2>/dev/null; then active=1; fi
  if [[ "$active" -eq 1 ]]; then
    require_drained "rewrite daemon.json (restarts dockerd)"
  fi
  if [[ "$CHECK" -eq 1 ]]; then
    PENDING=$((PENDING + 1))
    printf '    would: atomically merge managed keys into %s:\n%s\n' "$path" "$want"
  else
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
  if [[ "$active" -eq 1 ]]; then
    mut systemctl restart docker
  fi
}

step_service() { # C4
  log "step: docker service enabled+started (C4)"
  if [[ ! -d /run/systemd/system ]] || ! command -v systemctl >/dev/null 2>&1; then
    warn "no systemd; cannot enable/start docker here (bastion has systemd; container validation stops at install)"
    return 0
  fi
  if ! systemctl is-enabled -q docker 2>/dev/null; then mut systemctl enable docker; fi
  if ! systemctl is-active -q docker 2>/dev/null; then
    mut systemctl start docker
  else
    log "docker already active"
  fi
}

# -------------------------------------------------------------- post-checks
# Verify-only. In apply mode a failure is fatal; in check mode it is reported.

post_checks() {
  log "post-checks (verify-only)"
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
      warn "pin MISMATCH: want $s, have $(dpkg-query -W -f='${Version}' "$pkg" 2>/dev/null || echo none)"
      fail=1
    fi
  done
  local held; held="$(apt-mark showhold 2>/dev/null || true)"
  for pkg in docker-ce docker-ce-cli containerd.io docker-buildx-plugin docker-compose-plugin; do
    if printf '%s\n' "$held" | grep -qx "$pkg"; then
      log "hold OK: $pkg"
    else
      warn "hold MISSING: $pkg"
      fail=1
    fi
  done
  if [[ -f /etc/docker/daemon.json ]]; then
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
  if command -v docker >/dev/null 2>&1 && docker info >/dev/null 2>&1; then
    local driver cgroup
    driver="$(docker info --format '{{.CgroupDriver}}' 2>/dev/null || echo unknown)"
    cgroup="$(docker info --format '{{.CgroupVersion}}' 2>/dev/null || echo unknown)"
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
  else
    warn "dockerd not reachable; driver/cgroup checks deferred to live host"
  fi
  if command -v virsh >/dev/null 2>&1 \
     || dpkg -l 2>/dev/null | grep -Eq 'libvirt|qemu-kvm|qemu-system'; then
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
    --check) CHECK=1; log "CHECK mode: no persistent host changes; key-verification temp file is cleaned on exit" ;;
    "") : ;;
    *) die "unknown argument: $1 (see --help)" ;;
  esac
  [[ "$#" -le 1 ]] || die "unexpected extra argument: ${*:2} (see --help)"
  [[ "$(id -u)" -eq 0 ]] || die "must run as root (target user: ${VELNOR_C1_TARGET_USER})"
  [[ "$WANT_POOLS" == 0 || "$WANT_POOLS" == 1 ]] \
    || die "VELNOR_C1_DOCKER_POOLS must be 0 or 1"
  [[ "$ALLOW_RESTART" == 0 || "$ALLOW_RESTART" == 1 ]] \
    || die "VELNOR_C1_ALLOW_RESTART must be 0 or 1"
  trap cleanup EXIT
  log "C1 host-setup target: ${VELNOR_C1_TARGET_NAME} (${VELNOR_C1_TARGET_USER}@${VELNOR_C1_TARGET_HOST})"
  preflight_os
  preflight_routes
  preflight_work
  preflight_env
  step_timezone
  step_base_packages
  step_git_lfs_bat
  step_docker_key
  step_docker_repo
  step_docker_packages
  step_holds
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
    log "DONE: C1 host-setup converged"
  fi
}

if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
  main "$@"
fi
