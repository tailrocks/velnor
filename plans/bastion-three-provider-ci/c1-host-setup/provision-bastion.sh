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
#   VELNOR_C1_COSMETICS=1      opt-in cosmetic shell setup (default: off)
#   VELNOR_C1_DOCKER_POOLS=1   opt-in 172.30.0.0/16 daemon pools (default: off,
#                              selene-style omit; refused if routes conflict)
#   VELNOR_C1_ALLOW_RESTART=1  confirm dockerd restart with running containers
#                              (set only after draining jobs; never by default)
#   VELNOR_C1_TERMINFO_SRC=... terminfo source text for xterm-ghostty
#                              (cosmetics only; absent => warn-and-continue)
#   All pins in pins.env are overridable for re-resolves.
#
# NEVER in this script (structural, grep-verifiable):
#   no `apt-get upgrade/dist-upgrade`, no `autoremove`, no `docker system prune`,
#   no release upgrade, no mirror rewrite, no block-device/storage commands,
#   no firewall/SSH changes, no libvirt/KVM/QEMU, no per-repo reservations.
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
COSMETICS="${VELNOR_C1_COSMETICS:-0}"
WANT_POOLS="${VELNOR_C1_DOCKER_POOLS:-0}"

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

case "${1:-}" in
  --help|-h) usage; exit 0 ;;
  --check) CHECK=1; log "CHECK mode: read-only, no changes will be made" ;;
  "") : ;;
  *) die "unknown argument: $1 (see --help)" ;;
esac

[[ "$(id -u)" -eq 0 ]] || die "must run as root (target user: ${VELNOR_C1_TARGET_USER})"

# ---------------------------------------------------------------- pre-flight
# Read-only. Runs first in both modes; the route read precedes every pool
# decision and the running-work read precedes every drain gate.

POOL_CONFLICT=0

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
  local routes=""
  if command -v ip >/dev/null 2>&1; then
    routes="$(ip route show 2>/dev/null || true)"
  elif [[ -f /proc/net/route ]]; then
    routes="$(cat /proc/net/route)"
  else
    warn "no 'ip' and no /proc/net/route: route read impossible, pools refused"
    POOL_CONFLICT=1
    return 0
  fi
  printf '%s\n' "$routes"
  # Conflict test: 172.30.0.0/16 must not overlap any host route, and the
  # docker defaults must not already be claimed on the host.
  if printf '%s\n' "$routes" | grep -Eq '172\.30\.|172\.17\.|172\.18\.'; then
    warn "host route overlaps 172.30/16 or docker default ranges: pools refused"
    POOL_CONFLICT=1
  else
    log "no route conflict with 172.30.0.0/16"
  fi
}

running_containers() {
  if command -v docker >/dev/null 2>&1; then
    # NB: `docker ps` exits nonzero when dockerd is down; the `|| true`
    # keeps pipefail from aborting (wc still prints 0 on empty input).
    docker ps -q 2>/dev/null | wc -l | tr -d ' ' || true
  else
    echo 0
  fi
}

preflight_work() {
  log "pre-flight: running-work read"
  local n; n="$(running_containers)"
  log "running containers: $n"
  if [[ "$n" -gt 0 ]]; then
    docker ps --format '{{.ID}} {{.Image}} {{.Status}}' 2>/dev/null || true
  fi
  if [[ -d /run/systemd/system ]] && command -v systemctl >/dev/null 2>&1; then
    local units; units="$(systemctl list-units --type=service --state=running --plain --no-legend 'velnor*' 2>/dev/null || true)"
    if [[ -n "$units" ]]; then log "active velnor units:"; printf '%s\n' "$units"; fi
  fi
}

preflight_env() {
  log "pre-flight: environment"
  if [[ -f /sys/fs/cgroup/cgroup.controllers ]]; then
    log "cgroup v2 present"
  else
    warn "cgroup v2 NOT detected (verify-only here; post-check re-tests)"
  fi
  log "APT sources:"
  ls /etc/apt/sources.list.d/ 2>/dev/null || true
  [[ -f /etc/apt/sources.list ]] && cat /etc/apt/sources.list || true
  log "block devices (read-only baseline; script never touches storage):"
  if command -v lsblk >/dev/null 2>&1; then lsblk -o NAME,SIZE,TYPE,MOUNTPOINT 2>/dev/null || true; fi
}

# Drain gate: refuse to disturb running work unless the operator explicitly
# confirms (after draining) via VELNOR_C1_ALLOW_RESTART=1. Re-reads live.
require_drained() {
  local reason="$1"
  local n; n="$(running_containers)"
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

step_base_packages() { # B2 (+ C1 prereqs): bootstrap curl/gnupg first (key fetch needs them)
  log "step: base APT set, present-only (B2+C1)"
  ensure_present_pkgs ca-certificates curl gnupg
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

key_fprs() { # key_fprs <file> -> primary + sub fingerprints, one per line
  gpg --show-keys --with-colons "$1" 2>/dev/null | awk -F: '/^fpr:/ {print $10}'
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
  trap 'rm -f "${tmp:-}"' EXIT
  if [[ "$CHECK" -eq 1 ]]; then
    # Check mode still fetches+verifies (read-only network) so a bad key fails fast.
    log "run: curl -fsSL $VELNOR_C1_DOCKER_KEY_URL (verify-only)"
    curl -fsSL "$VELNOR_C1_DOCKER_KEY_URL" -o "$tmp"
  else
    log "run: curl -fsSL $VELNOR_C1_DOCKER_KEY_URL"
    curl -fsSL "$VELNOR_C1_DOCKER_KEY_URL" -o "$tmp"
  fi
  # NB: `|| true` so a corrupt download reaches the mismatch die() below, not a bare abort.
  local got; got="$(key_fprs "$tmp" || true)"
  local primary sub
  primary="$(printf '%s\n' "$got" | sed -n '1p')"
  sub="$(printf '%s\n' "$got" | sed -n '2p')"
  [[ "$primary" == "$VELNOR_C1_DOCKER_KEY_FPR" ]] \
    || die "docker key primary fingerprint mismatch: got ${primary:-none}, want $VELNOR_C1_DOCKER_KEY_FPR"
  [[ "$sub" == "$VELNOR_C1_DOCKER_KEY_SUB" ]] \
    || die "docker key subkey fingerprint mismatch: got ${sub:-none}, want $VELNOR_C1_DOCKER_KEY_SUB"
  log "key fingerprints OK: $primary / $sub"
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
  if [[ -f /etc/apt/sources.list.d/docker.list ]] \
     && [[ "$(</etc/apt/sources.list.d/docker.list)" == "$want" ]]; then
    log "repo file already current"
  else
    mut bash -c "printf '%s\n' \"$want\" > /etc/apt/sources.list.d/docker.list"
    mut chmod 0644 /etc/apt/sources.list.d/docker.list
    APT_UPDATED=0 # repo changed -> refresh before installs
  fi
  if [[ -e /etc/apt/sources.list.d/docker-ce.list ]]; then
    mut rm -f /etc/apt/sources.list.d/docker-ce.list
  fi
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
  mut apt-get install -y "${todo[@]}"
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
  log "NOTE: the C1 package transaction must explicitly 'apt-mark unhold velnor-runner' inside its locked procedure when upgrading the runner."
}

daemon_want() {
  if [[ "$WANT_POOLS" == "1" ]]; then
    printf '{\n  "log-opts": {\n    "max-size": "10m"\n  },\n  "default-address-pools": [\n    {\n      "base": "172.30.0.0/16",\n      "size": 24\n    }\n  ]\n}\n'
  else
    printf '{\n  "log-opts": {\n    "max-size": "10m"\n  }\n}\n'
  fi
}

step_daemon_json() { # C5: log size always; pools only if requested AND route-clear
  log "step: daemon.json (C5)"
  if [[ "$WANT_POOLS" == "1" ]]; then
    [[ "$POOL_CONFLICT" -eq 0 ]] \
      || die "VELNOR_C1_DOCKER_POOLS=1 but host routes conflict; refusing pools (spec §6: never alter the public route for container networks)"
    log "pools requested and route-clear: including 172.30.0.0/16"
  else
    log "pools not requested: selene-style omit (log limits only)"
  fi
  local want; want="$(daemon_want)"
  if [[ -f /etc/docker/daemon.json ]] && [[ "$(</etc/docker/daemon.json)" == "$want" ]]; then
    log "daemon.json already current"
    return 0
  fi
  local active=0
  if [[ -d /run/systemd/system ]] && command -v systemctl >/dev/null 2>&1 \
     && systemctl is-active -q docker 2>/dev/null; then active=1; fi
  if [[ "$active" -eq 1 ]]; then
    require_drained "rewrite daemon.json (restarts dockerd)"
  fi
  [[ -d /etc/docker ]] || mut install -m 0755 -d /etc/docker
  if [[ "$CHECK" -eq 1 ]]; then
    PENDING=$((PENDING + 1))
    printf '    would: write /etc/docker/daemon.json:\n%s\n' "$want"
  else
    log "run: write /etc/docker/daemon.json"
    printf '%s' "$want" > /etc/docker/daemon.json
    chmod 0644 /etc/docker/daemon.json
  fi
  if command -v python3 >/dev/null 2>&1 && [[ "$CHECK" -eq 0 ]]; then
    python3 -c 'import json,sys; json.load(open("/etc/docker/daemon.json"))' \
      || die "wrote invalid daemon.json"
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

# --------------------------------------------------------------- cosmetics
# Opt-in only (VELNOR_C1_COSMETICS=1). Deviations from the ansible source are
# deliberate: a cosmetic step must NEVER abort host setup (the source's
# terminfo fail-closed abort would brick an unattended bastion run), and the
# .zshrc copy is skipped (companion config/zshrc lives outside §6.1 scope).

step_cosmetics() { # B3 + B7
  if [[ "$COSMETICS" != "1" ]]; then
    log "step: cosmetics skipped (VELNOR_C1_COSMETICS!=1)"
    return 0
  fi
  log "step: cosmetics (B3+B7, opt-in)"

  # B3 terminfo: warn-and-continue when no source is provided.
  if command -v infocmp >/dev/null 2>&1 && infocmp xterm-ghostty >/dev/null 2>&1; then
    log "xterm-ghostty terminfo already present"
  elif [[ -z "${VELNOR_C1_TERMINFO_SRC:-}" ]]; then
    warn "cosmetic terminfo skipped: set VELNOR_C1_TERMINFO_SRC to install (non-fatal by design)"
  else
    mut bash -c 'printf "%s" "$VELNOR_C1_TERMINFO_SRC" > /tmp/xterm-ghostty.terminfo'
    mut tic -x /tmp/xterm-ghostty.terminfo
    mut rm -f /tmp/xterm-ghostty.terminfo
  fi

  # B7 oh-my-zsh (creates-guard; inherited curl|sh risk, cosmetics-gated).
  if [[ -d /root/.oh-my-zsh ]]; then
    log "oh-my-zsh already present"
  else
    mut bash -c 'sh -c "$(curl -fsSL https://raw.githubusercontent.com/ohmyzsh/ohmyzsh/master/tools/install.sh)" "" --unattended'
  fi

  # B7 zsh-autosuggestions, PINNED (source floats on master).
  local plug=/root/.oh-my-zsh/custom/plugins/zsh-autosuggestions
  if [[ -d "$plug" ]]; then
    local rev; rev="$(git -C "$plug" rev-parse HEAD 2>/dev/null || echo drifted)"
    if [[ "$rev" == "$VELNOR_C1_ZSH_AUTOSUGGESTIONS_PIN" ]]; then
      log "zsh-autosuggestions already pinned"
    else
      mut git -C "$plug" fetch origin
      mut git -C "$plug" checkout "$VELNOR_C1_ZSH_AUTOSUGGESTIONS_PIN"
    fi
  else
    mut git clone https://github.com/zsh-users/zsh-autosuggestions "$plug"
    mut git -C "$plug" checkout "$VELNOR_C1_ZSH_AUTOSUGGESTIONS_PIN"
  fi

  # B7 root shell (ordering-safe: zsh installed in the B2 core step).
  if ! command -v zsh >/dev/null 2>&1; then
    warn "zsh missing; skipping root-shell change"
  elif [[ "$(getent passwd root | cut -d: -f7)" == "/bin/zsh" ]]; then
    log "root shell already /bin/zsh"
  else
    mut chsh -s /bin/zsh root
  fi

  # B7 starship (creates-guard; inherited curl|sh risk, cosmetics-gated).
  if [[ -x /usr/local/bin/starship ]]; then
    log "starship already present"
  else
    mut bash -c 'curl -sS https://starship.rs/install.sh | sh -s -- -y'
  fi

  log ".zshrc copy skipped by design (companion config/zshrc is outside §6.1 scope)"
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
      python3 -c 'import json; json.load(open("/etc/docker/daemon.json"))' \
        && log "daemon.json valid JSON" || { warn "daemon.json INVALID"; fail=1; }
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
    [[ "$driver" == "systemd" ]] && log "cgroup driver: systemd" || { warn "cgroup driver: $driver"; fail=1; }
    [[ "$cgroup" == "2" ]] && log "cgroup version: 2" || { warn "cgroup version: $cgroup"; fail=1; }
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
  step_cosmetics
  post_checks
  if [[ "$CHECK" -eq 1 ]]; then
    if [[ "$PENDING" -gt 0 ]]; then
      log "CHECK result: $PENDING change(s) pending"
      exit 1
    fi
    log "CHECK result: converged, no changes pending"
  else
    log "DONE: C1 host-setup converged"
  fi
}

main
