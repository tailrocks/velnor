# C1 bastion host setup (authored, not executed)

This is the C1 preparation slice for `bastion` (`root@37.27.110.241`, Debian
13 Trixie, amd64). It contains an authoring-only provisioner. No command in
this change connected to or modified the bastion.

## Files

| File | Purpose |
| --- | --- |
| `provision-bastion.sh` | Read-first, idempotent provisioner with `--check` and guarded apply modes |
| `deploy-bastion.sh` | Authenticated, staged source upload with failure cleanup |
| `promote-staged-tree.sh` | Digest-checks and atomically promotes a staged source tree |
| `merge-daemon-json.py` | Validates and merges only C1-managed Docker daemon settings |
| `provision-permit-ledger-roster.py` | Creates and validates stock daemon permit-ledger, state, and slot paths |
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
3. Control host has Bash, OpenSSH client tools, `ssh-keygen`, `tar`, `shasum`,
   and the C1 files.
   The target must independently pass the script's Debian 13/Trixie/amd64 gate.
4. Recheck Docker versions against the official
   [Trixie amd64 package index](https://download.docker.com/linux/debian/dists/trixie/stable/binary-amd64/Packages)
   at the live gate. The pins were last checked on 2026-09-19. Docker's
   [Debian installation guide](https://docs.docker.com/engine/install/debian/)
   currently lists Trixie 13 and a repository `Signed-By` keyring.
5. Run `--check`, review the read-only inventory, then get the C-gate operator's
   explicit go-ahead before apply. Exit 2 means Docker or APT state is unknown;
   do not treat it as a resolved plan.

Use only the supplied host key file for every SSH/SCP call:

```bash
set -euo pipefail
C1=plans/bastion-three-provider-ci/c1-host-setup
HOST=37.27.110.241
KNOWN_HOSTS="${KNOWN_HOSTS:?set to the out-of-band-authenticated known_hosts file}"
test -f "$KNOWN_HOSTS" && test -s "$KNOWN_HOSTS"
ssh-keygen -F "$HOST" -f "$KNOWN_HOSTS" >/dev/null
export HOST KNOWN_HOSTS

"$C1/deploy-bastion.sh"

SSH_OPTS=(
  -o BatchMode=yes
  -o IdentitiesOnly=yes
  -o StrictHostKeyChecking=yes
  -o GlobalKnownHostsFile=/dev/null
  -o "UserKnownHostsFile=$KNOWN_HOSTS"
)

ssh "${SSH_OPTS[@]}" "root@$HOST" \
  'exec 9<>/root/.c1-host-setup.promote.lock &&
   /usr/bin/flock --shared --nonblock 9 &&
   export VELNOR_C1_PROMOTION_LOCK_FD=9 &&
   exec /bin/bash /root/c1-host-setup/provision-bastion.sh --check'
```

`StrictHostKeyChecking=yes` rejects an absent or changed key. The explicit
`UserKnownHostsFile` and disabled global file require the operator-supplied
entry. `ssh-keygen -F` checks that the entry exists; it does not authenticate
how the operator obtained it.

Stop after `--check`. Review its full output and get the C-gate operator's
go-ahead before applying. Then run:

```bash
set -euo pipefail
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
ssh "${SSH_OPTS[@]}" "root@$HOST" \
  'exec 9<>/root/.c1-host-setup.promote.lock &&
   /usr/bin/flock --shared --nonblock 9 &&
   export VELNOR_C1_PROMOTION_LOCK_FD=9 &&
   exec /bin/bash /root/c1-host-setup/provision-bastion.sh'
ssh "${SSH_OPTS[@]}" "root@$HOST" \
  'exec 9<>/root/.c1-host-setup.promote.lock &&
   /usr/bin/flock --shared --nonblock 9 &&
   export VELNOR_C1_PROMOTION_LOCK_FD=9 &&
   exec /bin/bash /root/c1-host-setup/provision-bastion.sh --check'
```

Each copy uses a SHA-256-checked archive and a validated remote staging path.
The control-host exit trap removes the local archive and attempts remote-stage
cleanup after upload or promotion failures. The remote promoter installs its
exit trap before checksum or extraction; on failure it removes staging and
restores the prior target when possible, retaining recovery paths if rollback
itself fails. It extracts a fresh release and atomically replaces the
`/root/c1-host-setup` symlink with that exact tree. The provisioner resolves
and holds the physical release directory and a shared root-owned promotion lock
for its full run; the promoter holds the same lock exclusively. The command
wrapper above opens FD 9 and acquires the shared lock before Bash opens the
active symlink, then exports that inherited FD for the provisioner’s early
entry check. Direct `bash /root/c1-host-setup/provision-bastion.sh` invocation
is rejected before the script reads release files. Same-directory `mv -T`
switches existing symlinks in one rename. A physical `/root/c1-host-setup`
directory is rejected untouched;
an operator must complete its one-time migration before using this deployer.
Repeated copies cannot nest the source directory or leave remote-only files in
the active tree. Prior symlink releases remain as hidden rollback copies under
`/root`.

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

When `velnor-runner` is installed, setup proves the default and configured
instance units match the pinned packaged fragment bytes, have no systemd
drop-ins, and resolve their identity, work path, and slot count from the stock
environment variables. Under the exclusive package lock and closed admission, it
provisions `/etc/velnor/permit-ledger.sources`, its mode-`0600` writer lock,
the shared permit ledger, every daemon state database, configured Scale Set
demand database, and native-slot marker directory before any captured daemon
can start. Empty databases are created mode `0600`; existing databases must
already be root-owned mode `0600` and are preserved. Slot directories use
mode `0750`. The default daemon gets four slot roots unless `VELNOR_SLOTS`
says otherwise. `--check` verifies every path and roster entry without
creating them; unresolved configuration or a non-stock service fails closed.
On an existing host, inspect each configured database and verify it is a
regular, non-symlink, root-owned file before changing its mode. If its mode is
not `0600`, run `chmod 0600 -- <exact-database-path>` for that state, ledger,
or demand database, then rerun `--check`. For example, the stock state path is
`/var/lib/velnor/state.db`. Use literal configured paths, never a glob; C1
does not change existing database modes or ownership.
The roster helper's standalone `--check` is read-only. Apply requires the
inherited exclusive package-lock descriptor and a verified stopped Velnor
unit inventory; use the provisioner for normal setup.

`--check` may fetch the Docker key into an ephemeral temporary file for
fingerprint verification; the cleanup trap removes it on exit. It reads target
state but makes no persistent host changes. In `--check`, a missing Docker CLI
or failed `docker info` is fatal as unknown. Apply may continue before Docker's
install/start stage only when systemd reports the service inactive, failed, or
not installed (or complete unit inventories confirm absence), and the
`dockerd` process table and Docker sockets are clear. Post-check then requires
a working CLI and `docker info`. If APT installation is pending, `--check` exits 2 as
unresolved instead of claiming exact pins and dependency resolution were
checked; it does not refresh APT lists or run the resolver. The local test
harness uses only temporary files and stubs.

The Docker key must contain exactly the expected primary and subkey records;
an appended key, extra subkey, malformed record, or fingerprint mismatch fails
before the key is installed. The repository remains scoped with `Signed-By`.
Docker package versions are exact pins and are held after installation.
Managed Docker APT keys, source files, and `daemon.json` must be root-owned,
single-link files with no group/world write bits or special permission bits.
Their parent configuration directories must also be root-owned and not
group/world writable; unsafe existing metadata fails before other host changes.
`/run/velnor` must be root-owned with the packaged tmpfiles mode `0750`; apply
repairs a missing runner lock only after closing admission and acquiring the
maintenance barrier, while `--check` reports that state as unresolved. Partial
`velnor-runner` dpkg states fail closed for operator repair.

`daemon.json` is parsed with duplicate-key rejection. The provisioner changes
only `log-opts.max-size` and, when explicitly requested, the exact C1 address
pool. It preserves every unrelated setting, refuses a conflicting existing
pool, symlink, special file, malformed JSON, or duplicate key, and writes by
same-directory atomic replacement while preserving mode and ownership.
Before its first APT or Docker configuration mutation, apply stops previously
active Velnor admission units and activation sockets/timers/paths, waits for
those units to stop, and checks Docker container inventory before it holds the exclusive
`/run/velnor/package-transaction.lock` through package changes, configuration
writes, Docker restart, and final Docker health checks. It releases that lock
before starting Velnor again because packaged Velnor services need a shared
lock during startup. On failure before Docker maintenance, cleanup tries to
restore the exact previously active units after unlocking; it leaves them
stopped if startup cannot be verified. After a Docker package or service
change, it reopens admission only after local Docker health is verified. An
unknown or unhealthy daemon leaves admission closed. Cleanup preserves the
original failure status. Package installation is not rolled back: any Docker
packages left installed after a partial APT failure are held for operator
review.
Running Docker containers block maintenance unless the operator explicitly
sets `VELNOR_C1_ALLOW_RESTART=1`. `VELNOR_C1_DRAIN_TIMEOUT_SECONDS` bounds
systemd stop operations and package-lock acquisition.
The same deadline bounds APT and systemd commands used during maintenance.

No shell cosmetics or remote `curl | sh` installers run. Every APT install and
`apt-mark` hold query, hold, or unhold uses the same package transaction lock.
The package hold for `velnor-runner` is reconciled in the separate candidate
APT transaction below.
Only an absent Docker source file or exact C1-managed content is accepted;
unknown content and a legacy `docker-ce.list` require operator review and are
never overwritten or deleted.

APT simulation is advisory; it does not serialize other APT or dpkg clients.
The provisioner records the selected package actions, including old version,
architecture, direction, and target version, then relies on APT's actual pre-install hook to
compare the transaction that APT is about to unpack. The hook also checks a
fingerprint of the private APT config, active source files and Docker key,
rechecks each package's candidate/origin policy, and hashes each downloaded
`.deb` against its signed package-index SHA-256. A source, candidate, action,
architecture, version, direction, or archive change fails before unpacking; removal and
downgrade actions are refused. Installing missing base packages also fails if
APT would change the version of any already-installed dependency. The fake-host
suite drives the hook with controlled inputs; it does not claim to run real
APT or prove APT lock behavior.

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
