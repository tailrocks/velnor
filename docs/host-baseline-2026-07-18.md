# Sentry host baseline — 2026-07-18

This is the Phase 0.5 denominator for every estate V-C timing claim. The host
is `sentry` (`/dev/md3`, XFS); inventory and cleanup ran on 2026-07-18 UTC.
Only paths and runtime objects owned by the configured Velnor daemons were
changed. No shared Docker image, volume, builder, or non-Velnor path was
deleted.

## Filesystem

| State | Used | Available | Use |
|---|---:|---:|---:|
| Before | 771 GiB | 149 GiB | 84% |
| After cache cleanup | 369 GiB | 551 GiB | 41% |
| After stale-workspace cleanup | 321,750,663,168 B | 664,653,627,392 B | 33% |

Inodes remained healthy (3% used before cleanup). The final root-filesystem
usage is well below plan 059's 60% ceiling.

## Velnor stores

The two invalid target identities accounted for 432,025,686,016 physical
bytes before cleanup:

| Deleted root | Before bytes |
|---|---:|
| `/var/lib/velnor/work/_velnor_targets/trusted/unknown-repository` | 233,025,220,608 |
| `/var/lib/velnor-jackin/work/_velnor_targets/trusted/unknown-repository` | 199,000,465,408 |

Plan 037's guarded GC first removed over-budget child scopes and wrote each
result to `/var/log/velnor/gc-history.jsonl`; the remaining validated roots
(195,555,975,168 B and 175,564,357,632 B respectively) were then removed by
their exact absolute paths while every daemon was stopped. Final inventory:
zero `unknown-repository` directories and zero bytes in all legacy target
roots.

Cargo, mise, and sccache were initially retained as legitimate warm stores.
The first live capacity acceptance exposed an over-broad shortfall calculation
and reclaimed the Java daemon's Cargo/mise/action caches. That defect was fixed
to reclaim only the measured shortfall; therefore the Java lane begins the
campaign cold while the other four daemon pools retain their warm stores. This
deviation must be considered in the first cold/warm timing pair.

Startup also found 64 completed-job UUID workspaces under configured
`slot-*` roots. The runner now removes only UUID-named directories directly
below its own slot roots at daemon startup. Verification after deployment:
zero UUID workspaces remained and root use fell from 40% to 33%.

## Docker and BuildKit

| Class | Before | After | Decision |
|---|---:|---:|---|
| Images | 39.24 GB | 39.24 GB | retained; no image-pull-cold reset |
| Containers | 11 total / 2 active | unchanged | no `velnor-job-*` leftovers existed |
| Volumes | 210.4 GB | 210.4 GB | retained; ownership was not exclusively Velnor |
| Build cache | 4.937 GB | 4.937 GB | retained; configured builders were shared or inactive |

There were zero `velnor-job-*` containers and zero `velnor-net-*` networks.
The exact `velnor-builder` definition was inactive with no backing container;
no broad Docker prune command was run.

## Fleet quiesce and restart

- All 13 estate repositories were inspected. Five stale Tablerock runs
  (`29622795974`, `29622795922`, `29622767235`, `29622767215`,
  `29622615516`) were canceled; the final non-completed count was zero.
- All five daemons drained, and the five repository scopes each reached zero
  runner registrations before cleanup.
- The hardened units improved `systemd-analyze security` from **9.4 UNSAFE**
  to **7.9 EXPOSED**. Live validation caught and fixed the missing writable
  `/root/.velnor/runner` JIT-state exception and a registration-retry drain
  regression.
- The host can safely hold 18 simultaneous default reservations (30 GiB each
  plus the 10 GiB emergency floor). Excess Java slots expose explicit capacity
  backpressure instead of creating broker sessions.

## Package delivery and remaining smoke gate

The operator reaffirmed that production deployment means the complete signed
apt path: pushed release commit and tag → Velnor `Release deb` →
`tailrocks/velnor-apt` Release → signed reprepro Pages publish → Sentry
`apt-get update && apt-get install velnor-runner`. Local artifact installation
used during initial host diagnosis is superseded and is not an accepted
deployment gate. Release `v0.1.35` is the first corrected package candidate.

The post-cleanup `compat.yml lanes=both` dispatch against fixture `main` was
rejected by GitHub with HTTP 422 because its current choice list does not yet
contain `both`. This is the documented plan-041 progress fallback, not a host
failure. Plan 059 remains open until plan 041 lands the canonical fixture input
and its new both-lane run URL is recorded here.

Operator-scope assertion: all destructive targets were Velnor-owned and were
validated by exact path/name before deletion; no non-Velnor resource was
touched.
