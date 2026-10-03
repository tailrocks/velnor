# Velnor fleet host operations (Phase 6)

Committed artifacts and operator steps. No live host access from CI — apply on
each runner host after package install.

## Storage root

Set on every fleet host so the canonical `v1` layout is used:

```bash
# /etc/velnor/velnor.env (merge from config/fleet/velnor-host.env after regen)
VELNOR_STORAGE_ROOT=/var
```

With the packaged default, stores live under:

```text
/var/cache/velnor/v1__trust_scope_v1/trust-scope-v1-<digest>/<class>/…
/var/lib/velnor/work/…
```

The keyed namespace is a sibling of the old `v1` root. Startup does not
migrate, reuse, or read old raw-scope directories under `v1`. The Debian
package's guarded `storage purge-legacy` step removes supported old roots only
for packaged instances still present in configuration. Undiscoverable custom
roots, roots for deconfigured instances, and unsupported old layouts may remain
outside the keyed catalog and still consume disk.

Record baseline after first GC:

```bash
velnorctl cache --work-dir /var/lib/velnor/work du
velnor-runner storage status   # when available on the host
```

## Scheduled cache GC

`velnor-runner` ships `velnor-cache-gc.timer` (alongside
`velnor-fleet-policy-audit.timer`) and its `postinst` enables it on install and
on every upgrade; `systemctl mask velnor-cache-gc.timer` is the opt-out the
package honours. An enabled timer never blocks an upgrade: the gc service is a
`Type=oneshot` wrapped in the shared package-transaction lock, and the
maintainer-script drain gates exempt exactly that class.

```bash
systemctl list-timers velnor-cache-gc.timer velnor-fleet-policy-audit.timer
```

The service runs one pass per packaged instance:

```bash
velnorctl cache gc --yes
```

Cache-GC budgets come from the `velnorctl cache gc` process environment
(`VELNOR_BUDGET_*`) and apply per class, not as one aggregate cache limit.
The packaged timer service does not load `/etc/velnor/velnor.env`; without a
service override, the command uses its compiled defaults. Packaged defaults
are 50 GiB for `actions-cache`, 200 GiB as a GC ceiling for retired `targets`
catalog roots, and 20 GiB each for the Cargo, mise, and artifact classes.
Active non-MBX
stable-workspace targets have a separate 30 GiB per-slot cap. Target each
configured store budget; alert when host available disk < 5 GiB. `cache du`
catalog rows cover known stores; its Docker usage estimate is separate and
appears when measurable.

## BuildKit GC (trusted docker hosts)

Copy `config/fleet/buildkitd.gc.toml` to the host BuildKit config path. The
managed `buildkitd-config-inline` input is limited to Velnor's reviewed
mirror-only stanza; it cannot carry host GC policy or arbitrary worker,
registry, or security settings. Keeps builder disk under ~40 GiB used with
10 GiB reserved and 5 GiB min free.

## BuildKit retention

Managed `docker/setup-buildx-action` steps use `cleanup: false`; generated
platform-build steps also set `keep-state: true`. Admission accepts only those
values when either input is present. Upstream v4.4.1 skips its own post cleanup
when `cleanup` is false, so `keep-state` alone has no effect there. Velnor's
native post adapter still releases that job's builder claim. With the managed
`cleanup: false` setting, it leaves the daemon running even after the last
claim is released. A later horizon pass can stop an unclaimed daemon and
eventually remove its builder and `_state` volume. `keep-state` does not
control that behavior.

Buildx setup triggers a best-effort BuildKit horizon pass when its six-hour
interval is due; startup and doctor checks also run a pass. For
current-generation managed builders, it stops an unclaimed running daemon and
deletes a stopped daemon and its state volume after seven idle days. If a
current-generation builder's daemon container is missing, the stale
registration may be removed after ownership and zero-holder checks. Retired
builder generations remain quarantined for explicit operator cleanup; Velnor
does not infer their Engine or storage identity. Below the 2 GiB free-space
floor, disk-pressure reclamation may remove eligible Velnor file-store entries.
It considers BuildKit pruning only if a fresh free-space sample after that
reclaim remains below the floor; pruning stops the daemon while preserving the
builder and state volume. At 90% measured usage it only
considers orphaned UUID job workspaces under work roots on the pressured
filesystem; that pass preserves file-store and BuildKit caches. The BuildKit
path requires a native Linux Docker Engine and proof that its state volume is
on the pressured filesystem; it is unavailable for the macOS Docker VM host.
Liveness and mount checks must pass. It does not prune dangling images.

Each low-space episode is durable in the service journal and keyed by the
Unix device identity; the filesystem UUID binds cleanup to that volume
incarnation. The first low sample persists a fixed 10-minute degraded
deadline (D) and a later 5-minute drain deadline (E = D + 5 minutes).
Relaunches reuse both deadlines. While an episode is active, the controller
stops idle waiter children and withholds permit grants and recovery. At D,
`Drain` refuses new work and waits up to 60 seconds before rechecking the
persisted episode and filesystem. A still-low observation at or after E
latches terminal pressure; `Deregister` runs failed-slot cleanup, deleting
the registration and clearing local slot state. The controller fences slot
generations at terminal and clears the episode only after every configured
root is freshly healthy, every desired slot is Fenced, and no jobs remain
active. An unknown or unmeasurable root blocks admission, and that root is
never reclaimed. The controller may still reclaim a different low root only
when its filesystem and volume identities are fully pinned and attested. An
unwritable journal or failed identity/liveness proof blocks cleanup. Clock
rollback or a filesystem identity change also fails closed. A reclaim
claim is committed before deletion, so a crash after the claim may consume the
one attempt without reclaiming data; the deadlines still bound the episode.

### Pre-guard appended BuildKit nodes

Buildx `--append` could create `node-N` daemon containers and `_state` volumes
before Velnor fenced appended-node creation. Those old child nodes have no
durable Velnor domain owner record. The automatic reaper handles only an
attested node 0, so child nodes remain quarantined. A Velnor-looking name alone
does not prove Engine, workflow, or volume ownership. Never use `docker rm` or
`docker volume rm` with a wildcard, and do not run `docker system prune` to
clear these objects.

First identify the exact builder and node indexes from the workflow setup
configuration/logs, then inspect Buildx and Docker inventory:

```bash
BUILDER='velnor-builder-<exact-builder-name>'
docker buildx ls
docker buildx inspect "$BUILDER"
docker ps -a --no-trunc --format '{{.ID}}\t{{.Names}}'
docker volume ls --format '{{.Name}}'
```

For each child node shown by `buildx inspect`, set its exact numeric index and
inspect both derived objects. Buildx names them
`buildx_buildkit_${BUILDER}${NODE_INDEX}` and that container name plus
`_state`:

```bash
NODE_INDEX='1'
CONTAINER="buildx_buildkit_${BUILDER}${NODE_INDEX}"
VOLUME="${CONTAINER}_state"
docker inspect --type container "$CONTAINER" \
  --format '{{.Id}} {{.Name}} {{.State.Status}} {{json .Config.Labels}} {{json .Mounts}}'
docker volume inspect "$VOLUME" \
  --format '{{.Name}} {{.Driver}} {{json .Labels}} {{.Mountpoint}}'
docker ps -a --filter "volume=$VOLUME" --no-trunc \
  --format '{{.ID}}\t{{.Names}}\t{{.Status}}'
```

Only remove an object after the workflow owner confirms that exact builder is
retired, Buildx reports no active node use, Docker inspection identifies the
same Engine and state volume, and no container uses the volume. Stop and remove
one exact container, then remove its one exact state volume:

```bash
docker stop "$CONTAINER"   # only if the inspected container is running
docker rm "$CONTAINER"
docker volume rm "$VOLUME"
```

If any identity or liveness check is unclear, leave both objects in place and
escalate with the builder name, node index, container ID, Engine ID, and volume
inspection output. Do not infer ownership from the `velnor-builder-` prefix.

## Trust-scope `pr` seed (D18)

Same-repo **pull_request** jobs on a **trusted** pool (`VELNOR_TRUST_SCOPE=trusted`):

- **Write scope:** `pr` — the admitted store scope for the PR. Cargo registry
  downloads are shared across slots. Cargo executable bins and mise installs
  and per-version binaries use sanitized `github.repository`; Cargo registry/
  Git downloads and mise download cache use trust scope only. MBX and sccache
  use numeric `github.repository_id`; Mr Boxington keeps cache and target
  subtrees per slot.
- **Seed:** before the job container starts, the daemon attempts to copy
  eligible missing regular-file units from the `trusted` Cargo store
  (`registry/cache`, `registry/index`, `git/db`) into `pr`. Registry files are
  individual budget units; each bare repository under `git/db` is one unit.
  Newest units are considered first. The seed cap is best-effort: when both
  the store-budget probe and destination size walk succeed, it is the compiler
  store budget minus existing `pr` Cargo data (floored at zero). If the budget
  probe fails, the seed is unbounded. If measuring the destination fails, the
  current size is treated as zero, granting the full nominal compiler-store
  budget as headroom. Each destination file is staged beside its destination
  and published without replacing an existing `pr` path. Readers see a
  complete file, and the copy never shares an inode with `trusted`. Symlinks
  found during enumeration are skipped. This is per-file publication, not a
  whole-store transaction: a later copy error can leave earlier files seeded.

Fork and unknown jobs remain on the `untrusted` floor. Trusted events (main
push, schedule, dispatch on default branch) write the `trusted` scope only.

**Hooks:** admission derives scope and seed source in
`trust_class::AdmittedTrust`; `storage::seed_cargo_store` copies before
`github_job_container_spec` builds the mounts (`runner.rs`,
`execute_script_job_inner`).

**Verify:** the daemon log shows `seeded pr cargo store from trusted: N files,
M bytes, T ms` per successful PR seed. MBX reuse is per slot, so compare jobs
on the same slot and unchanged source. Trusted non-PR jobs write to `trusted`;
cache accounting and GC include both the `pr` and `trusted` keyed roots.
`cache du` lists a separate path for each root; the digest does not spell the
scope label.
