# C1 bastion host setup (authored, not executed)

This is the C1 preparation slice for `bastion` (`root@37.27.110.241`, Debian
13 Trixie, amd64). It contains an authoring-only provisioner. No command in
this change connected to or modified the bastion.

## Files

| File | Purpose |
| --- | --- |
| `provision-bastion.sh` | Read-first, idempotent provisioner with `--check` and guarded apply modes |
| `merge-daemon-json.py` | Validates and merges only C1-managed Docker daemon settings |
| `pins.env` | Exact Docker package versions, repository, and authenticated key fingerprints |
| `targets.env` | Bastion identity for display; contains no SSH trust-on-first-use options |
| `tests/` | Local syntax, lint, and fake-host rejection/idempotence checks |

## Preconditions for a future live C gate

1. B4 and C dependencies are green; this preparation does not authorize host
   execution.
2. The operator has authenticated the bastion's SSH host key through an
   independent trusted channel and placed it in a private `known_hosts` file.
   Never create this file by accepting an unverified first connection or by
   trusting `ssh-keyscan` output alone.
3. Control host has Bash, OpenSSH client tools, `ssh-keygen`, and the C1 files.
   The target must independently pass the script's Debian 13/Trixie/amd64 gate.
4. Recheck Docker versions against the official
   [Trixie amd64 package index](https://download.docker.com/linux/debian/dists/trixie/stable/binary-amd64/Packages)
   at the live gate. The pins were last checked on 2026-09-19. Docker's
   [Debian installation guide](https://docs.docker.com/engine/install/debian/)
   currently lists Trixie 13 and a repository `Signed-By` keyring.
5. Run `--check`, review every planned change and the read-only inventory, then
   get the C-gate operator's explicit go-ahead before apply.

Use only the supplied host key file for every SSH/SCP call:

```bash
C1=plans/bastion-three-provider-ci/c1-host-setup
HOST=37.27.110.241
KNOWN_HOSTS="${KNOWN_HOSTS:?set to the out-of-band-authenticated known_hosts file}"
test -f "$KNOWN_HOSTS" && test -s "$KNOWN_HOSTS"
ssh-keygen -F "$HOST" -f "$KNOWN_HOSTS" >/dev/null
SSH_OPTS=(
  -o BatchMode=yes
  -o IdentitiesOnly=yes
  -o StrictHostKeyChecking=yes
  -o GlobalKnownHostsFile=/dev/null
  -o "UserKnownHostsFile=$KNOWN_HOSTS"
)

scp "${SSH_OPTS[@]}" -r "$C1" "root@$HOST:/root/c1-host-setup"
ssh "${SSH_OPTS[@]}" "root@$HOST" \
  bash /root/c1-host-setup/provision-bastion.sh --check
ssh "${SSH_OPTS[@]}" "root@$HOST" \
  bash /root/c1-host-setup/provision-bastion.sh
ssh "${SSH_OPTS[@]}" "root@$HOST" \
  bash /root/c1-host-setup/provision-bastion.sh --check
```

`StrictHostKeyChecking=yes` rejects an absent or changed key. The explicit
`UserKnownHostsFile` and disabled global file require the operator-supplied
entry. `ssh-keygen -F` checks that the entry exists; it does not authenticate
how the operator obtained it.

## Provisioner behavior

The read-only preflight records OS/architecture, full IPv4 routes, running
containers and Velnor units, CPU/NUMA/RAM, block devices and filesystems,
mounts, disk and inode usage, listening sockets, firewall rules, APT sources,
and existing Docker daemon settings. The provisioner issues no direct firewall
policy command and never changes storage. Installing or starting Docker can add
Docker-managed netfilter rules, so compare the captured firewall state before
and after apply and review those changes at the live gate.
Pool conflicts use CIDR network overlap, including supernets and containment;
an unreadable or incomplete route table refuses requested pools.

`--check` may fetch the Docker key into an ephemeral temporary file for
fingerprint verification; the cleanup trap removes it on exit. It reads target
state but makes no persistent host changes. The local test harness uses only
temporary files and stubs.

The Docker key must contain exactly the expected primary and subkey records;
an appended key, extra subkey, malformed record, or fingerprint mismatch fails
before the key is installed. The repository remains scoped with `Signed-By`.
Docker package versions are exact pins and are held after installation.

`daemon.json` is parsed with duplicate-key rejection. The provisioner changes
only `log-opts.max-size` and, when explicitly requested, the exact C1 address
pool. It preserves every unrelated setting, refuses a conflicting existing
pool, symlink, special file, malformed JSON, or duplicate key, and writes by
same-directory atomic replacement while preserving mode and ownership.
Running Docker containers block package or daemon changes unless the operator
has drained work and explicitly sets `VELNOR_C1_ALLOW_RESTART=1`.

No shell cosmetics or remote `curl | sh` installers run. The package hold for
`velnor-runner` is reconciled in the separate candidate APT transaction below.
Only an absent Docker source file or exact C1-managed content is accepted;
unknown content and a legacy `docker-ce.list` require operator review and are
never overwritten or deleted.

## Locked Velnor package transaction

The setup holds `velnor-runner` when installed. The repository has package
publication and verification workflows, but no unattended bastion package
updater; the hold intentionally blocks ordinary `apt upgrade`. Operators must
use this manual exact-candidate transaction to update it. The lock-owning Bash
process stays an ancestor of apt, dpkg, and maintainer scripts, which is what
the package lock check verifies.

```bash
: "${VERSION:?set to the B4-verified candidate}"
export VERSION
/usr/bin/flock --exclusive --nonblock --no-fork \
  /run/velnor/package-transaction.lock \
  /bin/bash -euo pipefail -c '
    holds=$(apt-mark showhold)
    was_held=0
    if printf "%s\n" "$holds" | grep -qx velnor-runner; then
      was_held=1
    fi
    rehold_runner() {
      rc=$?
      trap - EXIT
      set +e
      status=$(dpkg-query -W -f="\${Status}" velnor-runner 2>/dev/null)
      if [ "$was_held" = 1 ] || printf "%s\n" "$status" | grep -Eq " (installed|unpacked|half-configured|half-installed)$"; then
        apt-mark hold velnor-runner || rc=1
      fi
      exit "$rc"
    }
    trap rehold_runner EXIT
    if [ "$was_held" = 1 ]; then
      apt-mark unhold velnor-runner
    fi
    apt-get install "velnor-runner=${VERSION}"
    apt-mark hold velnor-runner
    trap - EXIT
  '
```

The complete C1 install still requires the work-plan's independent repository
signature, metadata, artifact-attestation, installed-identity, and
`release verify-installed` checks. Never use `dpkg -i`, a copied executable,
or bypass APT signature verification.

## Local validation

Run `bash plans/bastion-three-provider-ci/c1-host-setup/tests/run.sh` on the
authoring host. It checks Bash syntax, ShellCheck, and fake-host rejection and
idempotence paths without accessing `/etc`, running APT, or connecting to a
host.

## Live Debian acceptance gate remains open

Local checks cannot establish target-specific state. Before C1 is accepted,
the live gate must still review the bastion's actual inventory and before/after firewall,
verify the trusted SSH host key, recheck package pins, run the read-only
`--check`, explicitly approve apply, drain affected jobs, apply, and prove a
second `--check` converges. It must then complete the locked APT candidate
transaction, installed-package identity checks, `release verify-installed`,
and package-derived activation and health checks. No live Debian acceptance
has been performed here.
