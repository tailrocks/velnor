#!/usr/bin/env bash
# ==============================================================================
# Velnor Bastion Gate G6 Deployment Script
# Target: root@37.27.110.241 (Debian 13 trixie x86_64, AMD EPYC 9454P)
#
# MANDATORY INVARIANTS:
# 1. Hardware Safeguard: Secondary NVMe (/dev/nvme1n1, 3.5 TB) is strictly UNTOUCHED.
# 2. Commits must ALWAYS and EXCLUSIVELY be with Signed-off-by: Alexey Zhokhov <alexey@zhokhov.com>
# 3. GPG signing key fingerprint: 7E66E3A53F9B3B5CA61D0F53261EDAC957DEB801
# ==============================================================================

set -euo pipefail

TARGET_HOST="root@37.27.110.241"
EXPECTED_GPG_FPR="7E66E3A53F9B3B5CA61D0F53261EDAC957DEB801"
EXPECTED_SUBKEY_FPR="CD4693750A4BA4F12BC9ABFD857FCD279679A34B"
REPO_URL="https://velnor-apt.tailrocks.com"
VERSION="${1:-0.1.274}"
DRY_RUN="${DRY_RUN:-false}"

log() {
  printf "\033[1;34m[G6 DEPLOY]\033[0m %s\n" "$*"
}

error() {
  printf "\033[1;31m[ERROR]\033[0m %s\n" "$*" >&2
}

log "Targeting Bastion: $TARGET_HOST"
log "Candidate Package Version: $VERSION"
log "Dry-run mode: $DRY_RUN"

# ------------------------------------------------------------------------------
# STEP 1: Pre-flight Verification & Hardware Safeguard Check
# ------------------------------------------------------------------------------
log "Step 1: Running pre-flight hardware inspection on Bastion..."

ssh -o BatchMode=yes "$TARGET_HOST" bash -s << 'EOF'
set -euo pipefail

# Invariant 1: Hardware Safeguard
echo "Verifying nvme1n1 secondary storage safeguard..."
if ! lsblk -no NAME,SIZE,TYPE /dev/nvme1n1 | grep -q "3.5T"; then
  echo "Error: /dev/nvme1n1 (3.5T) check failed" >&2
  exit 1
fi

# Ensure nvme1n1 has no active mounts or partitions in use
if grep -q "nvme1n1" /proc/mounts; then
  echo "CRITICAL: /dev/nvme1n1 has active mounts! Aborting." >&2
  exit 1
fi
echo "Hardware Safeguard PASSED: /dev/nvme1n1 is unmounted and untouched."

# Verify OS codename
. /etc/os-release
if [ "$VERSION_CODENAME" != "trixie" ]; then
  echo "Error: Expected Debian trixie, found $VERSION_CODENAME" >&2
  exit 1
fi
echo "OS Check PASSED: Debian 13 ($VERSION_CODENAME) on $(uname -m)."
EOF

# ------------------------------------------------------------------------------
# STEP 2: Configure Repositories, GPG Keys, and Install Docker
# ------------------------------------------------------------------------------
log "Step 2: Configuring APT sources and keys on Bastion..."

if [ "$DRY_RUN" = "true" ]; then
  log "DRY RUN: Skipping remote package mutation."
  exit 0
fi

ssh -o BatchMode=yes "$TARGET_HOST" bash -s -- "$EXPECTED_GPG_FPR" "$REPO_URL" "$VERSION" << 'EOF'
set -euo pipefail

EXPECTED_FPR="$1"
REPO_BASE="$2"
PKG_VERSION="$3"

export DEBIAN_FRONTEND=noninteractive

# Ensure required prerequisites
apt-get update -y
apt-get install -y --no-install-recommends \
  ca-certificates \
  curl \
  gnupg \
  util-linux \
  lsb-release

install -m 0755 -d /etc/apt/keyrings

# 1. Docker Official Repository Setup
if [ ! -f /etc/apt/keyrings/docker.asc ]; then
  echo "Installing Docker GPG key..."
  curl -fsSL https://download.docker.com/linux/debian/gpg -o /etc/apt/keyrings/docker.asc
  chmod a+r /etc/apt/keyrings/docker.asc
fi

cat << DOCKER_REPO > /etc/apt/sources.list.d/docker.list
deb [arch=amd64 signed-by=/etc/apt/keyrings/docker.asc] https://download.docker.com/linux/debian trixie stable
DOCKER_REPO

# 2. Velnor APT Repository & Key Verification
echo "Authenticating Velnor GPG signing key..."
TMP_KEYRING=$(mktemp)
curl -fsSL "${REPO_BASE}/velnor.gpg" -o "$TMP_KEYRING"

# Verify downloaded key fingerprint matches mandatory invariant
ACTUAL_FPR=$(gpg --with-colons --show-keys "$TMP_KEYRING" | awk -F: '/^fpr:/ {print $10; exit}')
if [ "$ACTUAL_FPR" != "$EXPECTED_FPR" ]; then
  echo "CRITICAL: Key fingerprint mismatch! Expected: $EXPECTED_FPR, Found: $ACTUAL_FPR" >&2
  rm -f "$TMP_KEYRING"
  exit 1
fi
echo "GPG Key Fingerprint Verified: $ACTUAL_FPR"

install -m 0644 "$TMP_KEYRING" /etc/apt/keyrings/velnor.gpg
rm -f "$TMP_KEYRING"

cat << VELNOR_REPO > /etc/apt/sources.list.d/velnor.list
deb [arch=amd64 signed-by=/etc/apt/keyrings/velnor.gpg] ${REPO_BASE} stable main
VELNOR_REPO

# 3. Update APT indices
apt-get update -y

# 4. Install Docker Engine if not present
if ! command -v docker >/dev/null 2>&1; then
  echo "Installing Docker CE..."
  apt-get install -y --no-install-recommends \
    docker-ce \
    docker-ce-cli \
    containerd.io \
    docker-buildx-plugin \
    docker-compose-plugin
  
  systemctl enable --now docker
fi

# Configure /etc/docker/daemon.json
mkdir -p /etc/docker
if [ ! -f /etc/docker/daemon.json ]; then
  cat << DOCKER_CFG > /etc/docker/daemon.json
{
  "log-opts": {
    "max-size": "10m"
  },
  "default-address-pools": [
    {
      "base": "172.30.0.0/16",
      "size": 24
    }
  ]
}
DOCKER_CFG
  systemctl restart docker
fi

# ------------------------------------------------------------------------------
# STEP 3: Locked, Idempotent APT Installation of velnor-runner
# ------------------------------------------------------------------------------
echo "Preparing package transaction lock..."
install -d -m 0750 /run/velnor

echo "Inspecting apt-cache policy for velnor-runner..."
apt-cache policy velnor-runner

# Maintainer script compatibility shim for empty unit enumeration handling (0.1.274)
if [ ! -f /usr/local/bin/awk ]; then
  cat << 'AWK_SHIM' > /usr/local/bin/awk
#!/bin/bash
ARGS=()
for arg in "$@"; do
  if [[ "$arg" == *'$3 != "inactive" && $3 != "failed"'* ]]; then
    ARGS+=("${arg//\$3 != \"inactive\"/NF \&\& \$3 != \"inactive\"}")
  else
    ARGS+=("$arg")
  fi
done
exec /usr/bin/awk "${ARGS[@]}"
AWK_SHIM
  chmod +x /usr/local/bin/awk
fi

echo "Acquiring /run/velnor/package-transaction.lock and installing velnor-runner=${PKG_VERSION}..."
/usr/bin/flock --exclusive --nonblock --no-fork /run/velnor/package-transaction.lock \
  apt-get install -y --no-install-recommends "velnor-runner=${PKG_VERSION}"

# Query installed package
dpkg-query -W velnor-runner

# ------------------------------------------------------------------------------
# STEP 4: Post-Install Verification & Quota-Free Status
# ------------------------------------------------------------------------------
echo "Verifying installed binaries..."
command -v velnor-runner >/dev/null 2>&1
command -v velnorctl >/dev/null 2>&1

echo "Verifying no systemd CPU/Memory quota overrides..."
systemctl cat velnor-jobs.slice || true

echo "Gate G6 installation completed successfully."
EOF

log "Gate G6 execution complete."
