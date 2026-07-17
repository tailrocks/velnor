# Cache GC Design

> **Superseded as the implementation contract:** this file documents the
> original read-only spike. The production design, canonical paths, live Sentry
> evidence, leases, filesystem-wide admission control, and delivery gates are
> in [storage-and-disk-pressure-2026-07-18.md](storage-and-disk-pressure-2026-07-18.md).
> Do not enable deletion from this spike without that design.

Velnor's warm-runner model keeps daemon-shared host stores under the work root:
`_velnor_cargo`, `_velnor_mise`, `_velnor_targets`, `_velnor_caches`,
`_velnor_artifacts`, and `_velnor_sccache`. The current daemon only parks slots
when free space falls below the disk floor. GC must run before that park decision
so a full host can reclaim old cache data instead of silently losing capacity.

## Policy

- `_velnor_targets`: keep the newest N buckets per
  `<trust-scope>/<repo>/<workflow>/<job-bucket>` scope. The bucket key already
  includes trust scope, repository, workflow, and job class, so GC must preserve
  that boundary and never deduplicate across it.
- `_velnor_caches`: honor the workflow retention window from
  `GITHUB_RETENTION_DAYS` when available, then apply a byte ceiling with LRU by
  last modified time. Entries are scoped by `<trust-scope>/<repo>`.
- `_velnor_artifacts`: honor `GITHUB_RETENTION_DAYS`, then the same byte ceiling
  policy. Artifacts are grouped by run key because downloads within a run expect
  the run's artifact namespace to stay coherent.
- `_velnor_cargo`: size-bounded LRU by top-level store area (`registry`, `git`,
  `bin`). Destructive GC needs a more precise Cargo-aware follow-up before
  deleting individual registry/git internals.
- `_velnor_mise`: size-bounded LRU by top-level store area (`installs`, `cache`).
  Executable installs remain scoped by trust/repo where the mount layout already
  requires that.
- `_velnor_sccache`: report size only. sccache has its own cache limits and
  should not be pruned by Velnor GC unless a later incident proves it necessary.

## Runtime Placement

The daemon should run a background reaper from the same pre-flight/health loop
that currently checks Docker and disk health. The order should be:

1. Build the active-job in-use set.
2. Run cache GC if `VELNOR_CACHE_GC=1`.
3. Re-check free disk space.
4. Park with `disk_space_problem` only if reclamation did not restore the floor.

That keeps the existing disk floor as the last resort while giving the daemon a
self-healing path before slots stop registering.

## Safety

This spike is read-only: `velnor-runner cache du` reports sizes and
`velnor-runner cache gc --dry-run` prints candidates without deleting. A
destructive follow-up must add or expose a daemon-shared store lock and take it
around the scan/delete phase. The current code relies on Cargo/mise/sccache file
locks and atomic cache saves, but there is no single Velnor GC lock available to
the daemon pre-flight context yet.

Destructive GC must also skip every active scope. The safe in-use set should come
from slot/job bookkeeping rather than directory names alone. Until that set is
reliable, destructive deletion stays disabled even when `VELNOR_CACHE_GC=1`.

## CLI Surface

- `velnor-runner cache du [--work-dir PATH] [--config-dir PATH]`: read-only
  report of total bytes per store and per scope.
- `velnor-runner cache gc --dry-run [--work-dir PATH] [--max-age-days N]
  [--max-size-bytes BYTES] [--keep-newest-targets N]`: candidate report only.
- Future destructive mode: `VELNOR_CACHE_GC=1 velnor-runner cache gc` may delete
  candidates only after the lock and in-use scope rules above are implemented and
  soaked.

## Open Questions

- Which slot-owned structure should expose the authoritative active
  `<trust>/<repo>/<workflow>/<job>` and `<trust>/<repo>` scopes to the reaper?
- Should `GITHUB_RETENTION_DAYS` be a hard minimum for artifacts/caches, or can
  the byte ceiling evict younger entries during emergency low-disk recovery?
- Should cargo registry/git GC become package-aware, or should operators rely on
  a coarse byte ceiling plus occasional manual cleanup?
- Should GC emit structured trace spans and a forensic log entry for every
  candidate/deletion before it is enabled in daemon mode?
