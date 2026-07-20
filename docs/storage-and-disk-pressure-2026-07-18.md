# Velnor Storage Contract and Sentry Disk-Pressure Investigation

Status: accepted direction and implementation plan (2026-07-18)  
Scope: the current 13-repository Rust, TypeScript, Java, Docker, and service-test estate  
Related: [master-plan.md](master-plan.md), [cache-gc-design.md](cache-gc-design.md), [rust-build-cache-hygiene-velnor.md](rust-build-cache-hygiene-velnor.md)

Strict local-only action configuration:
[strict-capability-contract.md](strict-capability-contract.md).

## Decision

Velnor must own a single, explicit storage contract and a filesystem-wide
capacity controller. Every byte Velnor persists must have a canonical path,
class, trust boundary, owner, budget, lifetime, last-use signal, and safe-delete
rule. Cache cleanup is an automatic runner responsibility, not an operator
runbook.

Keep capped **sccache** as the production Rust compiler cache. Evaluate
**kache** only in an isolated trusted canary after the host-level controller is
working. Kache is promising for content-addressed deduplication on Sentry's XFS
filesystem, but it neither owns the other large stores nor currently has a
proven supported topology for Velnor's concurrent container mounts.

No files, images, volumes, or caches were deleted during this investigation.

## Live Sentry evidence

Read-only inspection on 2026-07-18 found:

| Signal | Observation |
|---|---:|
| Root filesystem | XFS, 919 GiB total, 769 GiB used, 150 GiB available, **84% used** |
| XFS features | `reflink=1`, `ftype=1`, `crc=1`, `bigtime=1` |
| `/var/lib/velnor` | 310.4 GB physical |
| `/var/lib/velnor-jackin` | 224.9 GB physical |
| `/var/lib/docker` | 260.3 GB physical |
| Main `_velnor_targets` | about 233.0 GB physical |
| Jackin `_velnor_targets` | about 199.0 GB physical |
| Persistent targets combined | **about 432 GB physical** |
| Main + Jackin `_velnor_sccache` | about 21.6 GB |
| Largest BuildKit stores | blockchain-nodes about 97.1 GB; Velnor builder about 60.6 GB |
| Active workload at inspection | no Cargo/rustc job; only two BuildKit containers running |
| Open deleted files | no material large files found |

The target trees contain roughly 450,000 files. Incremental directories total
less than 1 GB, so incremental sessions are not the present primary consumer.
A basename-and-size sample found 15,333 duplicated Rust artifact keys, with
common artifacts repeated 20--32 times. This is not a content hash and does not
prove an exact reclaim amount, but it confirms substantial cross-bucket output
duplication.

Docker also contains application data unrelated to Velnor, including a roughly
26 GB SeaweedFS volume. The controller must never reclaim unowned Docker state.

## Root cause

The architecture assigned cache paths for warmth but did not assign complete
lifetime ownership. A persistent target "bucket" is a mutable accumulator, not
a bounded cache generation. Multiple daemon-specific work roots scatter state,
BuildKit stores live behind opaque Docker volume names, and the runner has no
global view of all consumers on the filesystem.

Three implementation defects make the current dry-run GC ineffective:

1. Live target paths use
   `trusted/unknown-repository/unknown-workflow/<job>`. Persistent identity is
   being read from a job-variable source that is absent in these messages.
   Falling back to `unknown-*` silently destroys the intended repository and
   workflow boundary.
2. The target store uses `scope_depth = 4` and `candidate_depth = 4`. The job
   directory is therefore both the retention scope and its only candidate.
   `keep-newest-targets = N` can never evict it. Descendant modification times
   also keep the sole accumulator perpetually young.
3. Destructive GC deliberately exits with "not implemented", and the CLI
   supplies an empty active-scope set. The default command also inspects
   `/root/.velnor/runner/_work`, while production daemons use several explicit
   `/var/lib/velnor*` roots.

The failure mode is structural: no policy currently guarantees that the sum of
all writable stores plus active-job peaks stays below filesystem capacity.

## Canonical on-disk contract

Implementation status: canonical root resolution, explicit legacy readability,
and fail-closed persistent identity are closed by plan 035. Capacity leases,
physical accounting, and destructive reconciliation remain plans 036–037.

Use one configurable prefix, defaulting to the normal Linux locations below.
Do not hide persistent caches under a daemon's GitHub-compatible work tree.

| Class | Default | Contents | Backup semantics |
|---|---|---|---|
| Configuration | `/etc/velnor/` | non-secret config and per-instance config | operator-managed |
| Secrets | `/etc/velnor/credentials/` | mode-restricted credentials | operator secret policy |
| Durable state | `/var/lib/velnor/` | SQLite catalog, instance identities, migrations | back up as required |
| Regenerable cache | `/var/cache/velnor/v1/` | every Velnor-owned cache below | never required for recovery |
| Job work | `/var/lib/velnor/work/` | active slot/job workspaces | disposable after ownership ends |
| Runtime | `/run/velnor/` | locks, leases, sockets, PIDs | tmpfs; recreated on boot |
| Logs | `/var/log/velnor/` | daemon, slot, GC, audit, trace logs | retention policy |

`VELNOR_STORAGE_ROOT` may relocate the whole data plane for a dedicated disk,
but individual ad-hoc `_velnor_*` path overrides are deprecated. A resolved
configuration command must print every effective path and its filesystem.

Canonical cache hierarchy:

```text
/var/cache/velnor/v1/
  <trust-scope>/
    cargo/{registry,git}/
    cargo/bin/<repository>/
    mise/{cache,installs/<repository>,rustup/<repository>}/
    compiler/sccache/
    compiler/kache/                 # canary only
    targets/<repository>/<workflow>/<job-class>/<contract-generation>/
    actions/<repository>/
    artifacts/<repository>/<run-id>/
    packages/{bun,gradle,uv}/<repository>/
    buildkit/<builder-id>/
```

Repository and workflow identity must come from normalized GitHub context
already trusted by the executor. Persistent storage creation must fail closed
if a required identity is unavailable; `unknown-*` is permitted only for an
ephemeral, job-private path. Path components are normalized and hashed where
necessary, while the catalog retains the human-readable value.

Mise executable installs and the rustup state used by mise's Rust backend
share the same trust/repository boundary. Persisting `/opt/mise/installs`
without `/root/.rustup` is invalid: it records a selected Rust version while
the actual compiler disappears with the job container. The job image seeds
both stores, and later jobs mount both before tool resolution.

The hierarchy is not permission by itself. Trust scope remains part of every
cache key and mount. Untrusted jobs never receive trusted writable stores,
compiler-cache credentials, executable tool stores, or the host Docker socket.

## Catalog and accounting

Implementation status: the read-only class catalog and `storage paths/status`
operator surface are closed by plan 035; the authoritative SQLite lifecycle,
allocated-byte accounting, budgets, and audit history remain plans 036–037.

Store an authoritative SQLite catalog under `/var/lib/velnor/storage.db`, with
the filesystem as the reconciled source of physical truth. Do not use atime.
Each cache object or managed scope records:

- stable ID, schema/contract generation, class, trust scope, repository,
  workflow, job class, creator instance, and physical path;
- logical bytes, periodically measured physical allocated bytes, inode count,
  creation, last-use, and last-reconciliation times;
- soft budget, hard budget, TTL, retention priority, regeneration cost, and
  current active lease count;
- deletion state and an append-only audit record with reason and bytes freed.

`velnor-runner storage status` becomes the normal operator view. It prints the
filesystem reserve, active reservations, bytes by class/scope, budget pressure,
oldest use, GC history, and unowned paths. `storage paths` prints the resolved
layout. `storage gc --dry-run` and destructive mode use exactly the same planner.
The older `cache du/gc --work-dir` interface becomes a compatibility alias.

Logical size alone is insufficient on reflink/hardlink filesystems. Capacity
decisions use `statvfs` free blocks and measured allocated bytes; logical bytes
remain useful for attribution.

## Active-job-safe reclamation

Implementation status: filesystem scope leases and lease-aware destructive GC
are closed by plans 036–037.

Every active job holds a kernel-backed shared lease for each mounted managed
scope. GC takes a non-blocking exclusive lease before deleting or rotating that
scope. The kernel releases leases after process death; a heartbeat and catalog
record provide diagnostics, not the sole safety mechanism. A filesystem-level
leader lock ensures that the five current daemon instances cannot run competing
reapers.

Target persistence changes from one immortal mutable directory to generations.
A job acquires the current compatible generation. GC can remove older unlocked
generations, and rotation gives a hot but oversized scope something reclaimable.
The compatibility contract includes Rust/toolchain identity, target triple,
profile/features class, relevant lockfile fingerprint, and cache schema. It
must not create a generation for every commit.

Workspaces have a job owner and lease. Normal completion removes them; daemon
startup and periodic reconciliation remove abandoned workspaces after a grace
period only when no live lease exists. This addresses the dozens of lingering
slot directories observed on Sentry.

## Filesystem-wide pressure controller

Implementation status: pre-session reservations, emergency reserve,
reclaim-before-advertise, and explicit backpressure are closed by plan 036;
class-budget and owned-builder reclamation are closed by plan 037.

One coordinator per filesystem polls free bytes/inodes, reacts after large
writes and job transitions, and uses hysteresis. Defaults must be bounded but
host-tunable:

- **target/free state:** reclaim until the configured target reserve is met;
- **soft pressure:** start background reclamation without disturbing active jobs;
- **hard pressure:** stop admitting new jobs, reclaim all inactive disposable
  classes, and preserve space for active jobs to finish and upload results;
- **emergency:** retain an absolute emergency reserve and reject new writes
  whose reservation would cross it.

Thresholds combine percentage and absolute bytes; percentages alone are unsafe
on both very large and small disks. Each job class has a conservative peak-disk
reservation learned from observed high-water marks with a configured floor.
Admission checks `free - active reservations - requested reservation`, so
parallel starts cannot collectively consume the last blocks.

### Reclaim before accept

The normal contract is **clean automatically, then accept**, not silently refuse
an assigned job:

```text
slot wants to advertise capacity
  -> reserve conservative worst-case bytes
  -> enough capacity? register the JIT slot
  -> otherwise run ordered GC toward reserve + job peak
  -> remeasure real free blocks and retry
  -> register only after the reservation is durable

job message arrives
  -> classify the actual workload and refine its reservation
  -> run a second targeted reclaim before starting its container
  -> hold the reservation and all cache leases for the entire job
  -> monitor growth; reclaim unlocked old data concurrently if headroom falls
  -> release reservation only after completion and result/log upload
```

This ordering matters because a GitHub job is already Velnor's responsibility
after acquisition; handing it back is not assumed to be a reliable scheduling
primitive. Velnor therefore gates **slot registration/advertisement** before it
can receive work. The fleet maintains enough already-reserved headroom for every
advertised idle slot. Once a job is acquired, Velnor runs it to completion and
uses emergency reclamation of inactive data if the estimate was low.

If cleanup cannot create a reservation because the remaining bytes are actively
leased or not owned by Velnor, the daemon does not register additional slots.
That is explicit backpressure before assignment, with a reason, required bytes,
reclaim attempts, and blocking owners in health/status output. It is not a
silent per-job refusal. An operator alert fires before all advertised capacity
disappears. Existing reserved jobs continue.

Reservation estimates are learned from physical high-water deltas by
repository/workflow/job class and use a high percentile plus safety margin,
with a configured worst-case floor for unknown classes. Large declared service
images, Docker builds, and target rotations add class-specific components.
Estimates decay only slowly after enough successful observations.

Reclamation order is based on ownership and regeneration cost:

1. orphaned workspaces and expired temporary transfers;
2. expired artifacts and Actions cache entries;
3. unlocked old target generations;
4. Velnor-owned inactive BuildKit records;
5. old images explicitly labeled as Velnor-owned;
6. compiler cache above its budget;
7. old Cargo/mise/package-manager downloads, with executable stores last.

After each class, remeasure actual free blocks. Never call unrestricted
`docker system prune`. Velnor creates named builders with Velnor ownership
labels and invokes builder-specific BuildKit pruning with its own keep-storage,
age, and active-build checks. Unlabeled volumes, images, and application data
are outside its authority.

This follows established control patterns without copying a foreign
orchestrator:

- BuildKit applies ordered GC from cheap/stale records toward broader cache and
  supports `reservedSpace`, `maxUsedSpace`, and `minFreeSpace`; Velnor configures
  those limits per owned builder and still verifies host free blocks afterward.
  [Docker BuildKit GC](https://docs.docker.com/build/cache/garbage-collection/)
- Kubernetes monitors both free bytes and free inodes on a short housekeeping
  interval and attempts node-level reclamation before workload eviction. Velnor
  adopts the signals and reclaim-first ordering, but does not evict an acquired
  CI job merely because a cache exceeded policy.
  [Kubernetes node-pressure eviction](https://kubernetes.io/docs/concepts/scheduling-eviction/node-pressure-eviction/)
- Kache and BuildKit both use hysteresis instead of cleaning exactly to the
  trigger line. Velnor similarly reclaims to a healthy target reserve so every
  arrival does not cause a tiny prune cycle.

An active job may run while inactive stores are reclaimed. If hard pressure
persists, do not kill it merely to recover cache space: stop admission, record
which reservation was exceeded, and preserve the completion/log-upload margin.
A genuinely abandoned process is first terminated through the normal bounded
supervisor drain, then its released scope becomes eligible.

## Rust compiler cache decision

Primary-source research inspected kache at commit `ee6f5d5` and sccache at
`4fcb161`. Context7 was unavailable for this repository, so current upstream
source and official project documentation were used.

Kache provides content-addressed blobs, SQLite WAL metadata, reflink/hardlink/
copy restoration, a default 50 GiB size limit, and automatic GC. Auto-GC checks
at most every five minutes, triggers above 110% of the limit, and evicts toward
90%. These are useful cache-local mechanisms, not a filesystem reserve.
[Kache configuration](https://github.com/kunobi-ninja/kache/blob/ee6f5d5d5abaffb39545cf166f8b105e9a3640e8/docs/getting-started/configuration.mdx)

The adoption blocker is topology. Kache's own documentation warns against
bind-mounting a host cache into a build container in the host-daemon/SQLite WAL
case and recommends a container-owned local cache context. Velnor currently
shares one host-persistent compiler store across concurrent job containers.
[Kache container guidance](https://github.com/kunobi-ninja/kache/blob/ee6f5d5d5abaffb39545cf166f8b105e9a3640e8/docs/getting-started/configuration.mdx#L306-L352)

Therefore:

1. keep explicitly capped sccache as default;
2. implement the host controller before any compiler-cache migration;
3. pin a stable kache version in a trusted canary image, not `latest`, with no
   Node-based `kache-action` product dependency;
4. test 1, 2, 4, and maximum parallel slots, concurrent GC/hit/put, cancellation,
   restart and reboot, and verify reflink behavior through the actual mounts;
5. compare physical allocated bytes, warm/cold duration, hit correctness, and
   failure recovery on the estate's small libraries, large Rust workspaces,
   native-heavy crates, and Docker-contained Rust builds;
6. require trust-scoped remote namespaces and credentials before any S3 trial.

Kache's `clean-targets` command is not fleet-aware and must never be used as
Velnor host GC. Its remote store also does not by itself establish producer
authenticity or a multi-tenant cache-poisoning boundary.

### Why kache remains a candidate

The candidate status is tied to Sentry's measured failure mode, not novelty:

1. The dominant Velnor-owned consumer is about 432 GB of persistent Cargo
   target trees, not the roughly 22 GB of sccache stores.
2. Common Rust artifacts appear in 20--32 target buckets. sccache avoids rustc
   work but its restored output becomes an ordinary independent target file;
   it does not make all resulting target trees share physical extents.
3. Kache stores content-addressed blobs and restores through reflink, then
   hardlink/copy fallbacks. Sentry's XFS reports `reflink=1`, so repeated
   immutable outputs may share extents between the Kache store and target
   buckets. This is a hypothesis until physical allocated bytes prove it
   through the actual Docker bind/overlay path.
4. Kache has an explicit local size policy, weighted eviction, access grace,
   GC serialization, and orphan sweeping. Those are useful subordinate
   mechanisms even though Velnor still owns the host reserve.

Kache loses candidate status if the topology soak finds SQLite/WAL corruption,
reflink restoration does not work through the real mount layout, physical bytes
do not materially fall, warm builds regress, or trust-scoped operation cannot
be made safe. The decision is evidence-gated rather than a committed migration.

### Supporting both compiler caches

Velnor should support both as **installed, selectable backends**, but not stack
both around one rustc invocation.

Cargo exposes one general `RUSTC_WRAPPER`; it can technically nest that with a
separate workspace wrapper, producing
`$RUSTC_WRAPPER $RUSTC_WORKSPACE_WRAPPER $RUSTC`. That mechanism does not prove
that two compiler caches are composable. Stacking sccache and kache would add
double lookup/write I/O, ambiguous hit and failure accounting, altered cache
keys, and two independent GC systems while providing little useful second-level
behavior. Sccache already has a supported multi-level backend chain when local
plus remote tiers are desired.
[Cargo wrapper configuration](https://doc.rust-lang.org/cargo/reference/config.html#buildrustc-wrapper),
[sccache configuration](https://github.com/mozilla/sccache/blob/main/docs/Configuration.md)

The supported modes should be:

| Mode | Purpose |
|---|---|
| `sccache` | production default; current estate behavior |
| `kache` | trusted canary and, only after gates pass, an optional production backend |
| `off` | diagnosis and correctness baseline |

Selection is immutable for a job and scoped by daemon/pool initially; later it
may be selected by trusted workload class. Sccache and kache use separate
canonical roots, budgets, leases, metrics, and remote credentials. Different
jobs may use different backends concurrently on the same host.

Implement a Rust `CompilerCacheBackend` seam owning binary discovery, mount,
environment, setup, post-job statistics, health, and cache-native GC. The
runner image may contain both pinned binaries, but it injects exactly one
wrapper. The existing native `mozilla-actions/sccache-action` adapter remains
GitHub-workflow compatible: on a Velnor kache canary it must clearly report that
the configured compiler-cache backend is kache and set the later-step
environment accordingly, rather than starting an unused sccache server.

The standardized workflows currently also set `RUSTC_WRAPPER=sccache` and run
an explicit `sccache --show-stats`. Those hard-coded commands prevent truthful
transparent backend selection. Standardization should retain the marketplace
sccache action for the GitHub lane but move wrapper selection and statistics
behind the setup action/post step; no workflow step should assume a cache CLI.
On GitHub-hosted this still resolves to sccache. On Velnor it allows the native
adapter to select the configured backend while keeping identical workload YAML.

Acceptance requires three independent fixture lanes using the same compile
workload: `off`, `sccache`, and `kache`. Add an explicit negative test proving
that configuration rejects simultaneous wrappers. Compare cold build, first
warm build, repeated warm build, physical target+store bytes, hit correctness,
concurrent GC, cancellation, and reboot recovery.

### Deep comparison: action and storage modes

The tools and their GitHub Actions integrations must not be conflated:

| Dimension | `mozilla-actions/sccache-action` | `kunobi-ninja/kache-action` | Consequence for Velnor |
|---|---|---|---|
| Compiler model | Mature client/server compiler wrapper | Newer content-addressed compiler wrapper with SQLite index | Both can be one selected `RUSTC_WRAPPER` |
| Local storage | Built-in disk backend, default 10 GiB unless configured | Content-addressed blob store, default 50 GiB unless configured | Override both with explicit, comparable budgets |
| Local restore | Materializes cached compiler outputs normally | Reflink, then hardlink/copy fallback | Kache may reduce physical target duplication on XFS |
| GitHub-cache mode | Sccache uses GitHub's cache service as an artifact backend when `SCCACHE_GHA_ENABLED=true` | Action restores/saves the **entire local Kache store** through `@actions/cache` when S3 is absent | Transfer granularity and setup/post time differ materially |
| Remote storage | S3/R2, GCS, Azure, Redis, Memcached, GHA, WebDAV, OSS, COS; multi-level chains with backfill | S3-compatible stores (AWS, R2, MinIO, Ceph); selective manifest/shard warm prefetch | Sccache has broader backend maturity; Kache has a specialized prefetch design |
| Action setup | Installs/starts sccache; workflow separately sets `RUSTC_WRAPPER` and GHA enablement | Installs Kache, sets wrapper, restores local store or starts S3 daemon | Velnor native abstraction must normalize setup semantics |
| Post-job data | Sccache statistics/annotations | Save/sync plus report, miss-cost breakdown, summary and optional PR comment | Capture a common metric schema and retain backend-native detail |
| Cache scope | Compiler invocations; Rust linker-producing crates and incremental builds have documented limitations | Per-crate content-addressed outputs; executable-like outputs are opt-in | Benchmark default-common scope first, then Kache executable mode separately |
| Path portability | Requires correct `SCCACHE_BASEDIRS` normalization | Claims normalized invocation keys portable across checkouts | Test different Velnor slot/workspace paths explicitly |
| GC | Local size cap; remote lifecycle is backend/operator policy | Weighted local eviction, grace, orphan sweep; S3 cap remains external | Both remain subordinate to Velnor filesystem GC |
| Project evidence | Long-lived project and already production-proven in this estate | Young project/action with limited production history | Kache needs stronger soak/failure gates, not dismissal |

Sources: [sccache project and storage support](https://github.com/mozilla/sccache),
[sccache action](https://github.com/mozilla-actions/sccache-action),
[Kache action mechanics](https://github.com/kunobi-ninja/kache-action).

The statement "sccache is remote, Kache is local hardlinks" is therefore
incomplete. Both have local and remote modes. Reflink/hardlink behavior is a
Kache local-store advantage, while remote reach and multi-level composition are
sccache advantages. On an ephemeral GitHub-hosted runner, Kache's local
deduplication helps during the job, but persistence still requires transferring
the store through GitHub cache or S3. On Velnor, the persistent XFS store makes
the local physical-byte question much more important.

### Required comparison experiment

Velnor should support both native actions and expose a temporary experiment
matrix. The approved first phase runs separate local-only jobs, never both
wrappers inside one job. Remote rows are research options requiring approval:

| Environment | Status and modes |
|---|---|
| Velnor persistent host | **Approved:** `off`, sccache-local, kache-local |
| GitHub-hosted | **Approved:** the same local-only configurations as compatibility/cold baselines |
| Velnor shared remote | **Not approved:** possible future same-S3 comparison |
| GitHub cache/remote | **Not approved:** possible future sccache-GHA/Kache-GHA or same-S3 comparison |

Local and remote stores are isolated by backend, experiment, trust scope, and
workload. They receive equal local byte ceilings; remote lifecycle policies and
network location are equal. Neither backend is allowed to consume the other's
warmed outputs. Exact stable binary and action commits are pinned.

Run the actual estate workload classes, not a synthetic hello-world:

1. small libraries: schemalane, pg-bigdecimal, tracing-request-level;
2. application/workspace: Velnor, holla, ruxel or termrock;
3. large/mixed workspace: jackin, java-monorepo, parallax;
4. native-heavy Rust builds and Rust compilation inside Docker builds.

For each representative workload, execute in fixed order:

1. empty store + empty target (cold correctness baseline);
2. warm store + empty target (true compiler-cache restore value);
3. warm store + retained target (normal warm Velnor value);
4. unchanged rerun;
5. workspace-source-only change;
6. one dependency/lockfile change;
7. same inputs from a different slot/workspace path;
8. concurrency at 1, 2, 4, and host maximum;
9. GC during lookup/write, cancellation, daemon restart, and reboot recovery.

Use the same Rust/mise lock, linker, flags, target triple, features,
`CARGO_INCREMENTAL=0`, CPU/memory limits, source commit, and job order. Rotate
backend order between repetitions to reduce thermal and host-load bias. Run
enough repetitions to report median and tail behavior rather than one best run.

Capture these numbers mechanically in a common Velnor result record:

- queue-to-start, setup/restore, compile, post/save, and total wall time;
- rustc requests, executable requests, local/remote hits, misses, non-cacheable
  reasons, hit latency, compile time avoided, and backend errors/fallbacks;
- network bytes/requests and remote storage bytes/objects;
- logical and physical allocated bytes for target, compiler store, and their
  combined footprint; inode count and dedup/reflink ratio;
- host free-space delta, GC duration, candidates, actual blocks freed, and
  subsequent cold-cost caused by eviction;
- output/test correctness, artifact hashes where deterministic, and cache
  behavior after corruption/cancellation/reboot.

The primary decision is a Pareto result, not hit rate alone. A backend wins a
workload only if it preserves correctness and host safety while improving
queue-to-result and/or physical bytes. Publish results by workload class: Kache
may be the better persistent-XFS backend while sccache remains better on
GitHub-hosted or for remote/backend breadth. Supporting both permanently is a
valid outcome when the measured winners differ.

The first approved experiment is local-only. The GHA/S3 rows above document
possible future comparisons, not approved features. Do not implement them
without separate approval. Exact allowed inputs and versions are normative in
[strict-capability-contract.md](strict-capability-contract.md).

## Delivery sequence and acceptance gates

1. Fix persistent identity and fail-closed path creation; add migration/report
   tooling for existing `unknown-*` stores.
2. Introduce the path resolver, storage config, catalog, `storage paths/status`,
   and discovery of legacy roots and Velnor-owned BuildKit builders.
3. Add leases, global coordinator lock, workspace reconciliation, target
   generations, and a tested destructive planner.
4. Add per-class budgets, soft/hard/emergency thresholds, learned job
   reservations, **reclaim-before-register**, BuildKit-specific pruning, audit
   logs, and tracing spans.
5. Migrate legacy data one daemon/trust scope at a time using reflink-or-copy,
   validate warm behavior, then remove the old tree only through the planner.
6. Run pressure tests with concurrent jobs until injected low-space conditions
   recover above target reserve without deleting an active scope or unrelated
   Docker data. Test crash/reboot recovery and five-daemon contention.
7. Only then run the kache canary gates.

Release acceptance requires bounded steady-state disk use under repeated runs,
no ENOSPC, no active-cache deletion, deterministic admission under simultaneous
starts, complete deletion forensics, correct trust isolation, and a warm rerun
that retains the no-download/no-dependency-recompile contract.

A low-space acceptance test must dispatch work while reclaimable old data
exists and prove that Velnor automatically reaches the required reserve,
advertises the slot, starts the job, and completes it without operator action.
A second test makes all remaining space actively leased and proves that no new
slot is advertised, health output names the exact constraint, and every already
acquired job completes safely.
