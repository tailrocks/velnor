# Plan 037: Destructive cache GC + physical accounting — finish the 029 spike

> **Executor instructions**: Follow step by step; run every verification; STOP
> conditions binding. Update the status row in `plans/README.md` when done.
>
> **Drift check (run first)**:
> `git diff --stat 48b04ad..HEAD -- crates/velnor-runner/src/cache.rs crates/velnor-runner/src/storage.rs`
> Re-locate by symbol; mismatch → STOP.

## Status

- **Priority**: P0 (V0.7 + V0.8; storage doc "Active-job-safe reclamation" +
  "Catalog and accounting"; successor of DONE spike plan 029)
- **Effort**: M/L
- **Risk**: HIGH (deletes data)
- **Depends on**: plans/035-canonical-storage-contract.md (path authority);
  couples with plans/036-capacity-controller-reclaim.md (livens the engine)
- **Category**: stability
- **Planned at**: commit `48b04ad`, 2026-07-18

## Why this matters

Warm stores without a lifetime owner filled Sentry to 84% (root cause class:
"caches created for speed with no lifetime owner"). The GC spike (plan 029)
landed a `cache` subcommand with a full eviction POLICY — but destruction is
stubbed (`bail!` unless `--dry-run`), in-use protection is a dead parameter,
accounting is logical-bytes only, and nothing protects against two daemons
reaping concurrently. Thirteen repos defaulting to Velnor multiplies write
pressure; this is a mass-flip stability gate, not polish.

## Current state (verified against `48b04ad`)

- `crates/velnor-runner/src/cache.rs:50-53` — verified:
  ```rust
  fn run_gc(work_root: &Path, args: CacheGcArgs) -> Result<()> {
      if !args.dry_run {
          bail!("destructive cache gc is not implemented in this spike; pass --dry-run");
      }
  ```
- `cache.rs:56-66` — `EvictionPolicy { now, keep_newest_per_target_scope,
  max_age, max_total_bytes, in_use_scopes: BTreeSet::new() }` — in-use set
  hardcoded empty at the call site.
- `cache.rs:308-393` — `select_eviction_candidates`: TTL
  (`older-than-max-age`), per-target-scope LRU (`keep_newest_per_target_scope`),
  byte-ceiling oldest-first. Mtime-based.
- `cache.rs:32-48` — `run_du`: logical bytes via recursive `dir_size`
  (`:221-251`); no statvfs/allocated-bytes.
- `cache.rs:96-141` — `store_roots()`; sccache `gc_managed: false` (:138);
  no BuildKit store entry.
- Tests: `cache.rs:429-576` (`cache_gc_keeps_newest_target_buckets_per_scope`
  :460, `cache_gc_skips_in_use_scopes` :551).
- No locks anywhere (grep-confirmed). CLI: `Command::Cache` with `Du`/`Gc`
  (`cli.rs:16,35-58`).

## Commands you will need

| Purpose | Command | Expected |
|---------|---------|----------|
| Gates | fmt / clippy / `cargo nextest run --workspace --locked` | exit 0 |
| Focused | `cargo nextest run -p velnor-runner cache` | pass |
| Manual (dev) | `cargo run -p velnor-runner -- cache gc --dry-run` | plan table prints |

## Scope

**In scope**: `cache.rs` (destruction, lock, physical accounting, budgets),
callable engine API for plan 036, BuildKit owned-prune hook, doctor `cache du`
enrichment, `cli.rs` flags.
**Out of scope**: lease creation (036 owns leases; this plan CONSUMES a
provided `in_use_scopes` set and takes the reaper leader-lock only),
storage-layout changes (035), sccache internal eviction (sccache manages its
own size cap — remains `gc_managed: false`; the runner only enforces
`SCCACHE_CACHE_SIZE`).

## Git workflow

Branch `velnor-estate-standard`; `feat(runner):` commits, `git commit -s`; no push
without operator instruction.

## Steps

### Step 1: Reaper leader lock

One lock file under the storage layout's run root (035's `storage.rs`):
`<run_root>/gc.lock`, `flock`-style exclusive (use `fs2` or `rustix` flock —
match whichever the workspace already depends on; check `Cargo.lock`, else
add `fs2` to `crates/velnor-runner/Cargo.toml`). Second reaper exits cleanly
with "another gc holds the lock".

**Verify**: test with two threads/processes on a tempdir lock.

### Step 2: Destruction engine

Replace the `bail!` at `cache.rs:50-53`: after candidate selection, without
`--dry-run`, delete each candidate scope dir bottom-up
(`fs::remove_dir_all`), collecting per-candidate results; a failed deletion
logs and continues (partial GC is fine). Forensics: one line per deleted
scope — class, scope key, logical bytes, reason
(`older-than-max-age`/`keep-newest`/`over-byte-ceiling`/`reclaim-target`),
policy values — to stderr AND a `gc-history.jsonl` under the log root.
Add `--yes` as a required confirmation flag for destructive runs from the
CLI (the library API for 036 passes it implicitly).

**Verify**: tempdir test `gc_deletes_selected_and_logs`
(build a fake store tree, run engine, assert removed dirs + history lines);
`gc_without_yes_refuses`.

### Step 3: Engine API for the controller

`pub fn reclaim(layout: &StorageLayout, target_bytes: u64, in_use: &BTreeSet<String>) -> Result<ReclaimReport>`
— candidate selection ordered regenerable-first (caches → targets → kache →
over-budget portions), stops when freed ≥ target, honors `in_use`. This is
what plan 036 step 3 calls.

**Verify**: `reclaim_stops_at_target_bytes`, `reclaim_skips_in_use` (the
existing `cache_gc_skips_in_use_scopes` pattern, now via the API).

### Step 4: Physical accounting + budgets

`run_du`: add allocated bytes per scope (`std::os::unix::fs::MetadataExt`
`blocks() * 512`) alongside logical; add per-class budget columns (budgets
from CLI/env: `--budget-targets-bytes` etc., env `VELNOR_BUDGET_*`; defaults
from the storage doc's per-class table — read the doc's "Catalog and
accounting" for the classes; where the doc gives no number, default 0 =
unbudgeted, print `-`). High-water flag when class > budget. Add a BuildKit
row: if a Velnor-owned builder exists (name it `velnor-builder` — creation is
out of scope; detect by `docker buildx ls` parse, tolerate absence), report
its size via `docker system df --format json` builder cache entry, and
implement `prune_owned_builder(target_bytes)` using
`docker buildx prune --builder velnor-builder --max-used-space` semantics —
NEVER `docker system prune`.

**Verify**: du output on a tempdir shows logical+physical columns; unit test
for the budget flagging; builder-absent path returns cleanly.

### Step 5: Doctor + docs

`doctor` includes a one-line cache summary (total logical/physical, worst
class vs budget). Annotate `docs/cache-gc-design.md` +
`docs/storage-and-disk-pressure-2026-07-18.md` implemented lines
(`(closed: plan 037)`).

**Verify**: doctor prints the line; grep annotations.

## Test plan

≥ 7 new tests as listed; existing `cache.rs` tests stay green; full gates.
Operator acceptance (with 036): on Sentry, dry-run plan matches expectation,
then a supervised destructive run reclaims the legacy `unknown-repository`
trees (post-035 those are no longer written) — record freed bytes.

## Done criteria

- [ ] Gates exit 0; the `bail!` stub is gone; `--yes` guard + leader lock in place
- [ ] `reclaim()` API used by 036 (or ready if 036 unlanded)
- [ ] du reports logical + physical + budget flags; BuildKit owned-prune only
- [ ] `grep -rn "system prune" crates/` → none
- [ ] No out-of-scope changes

## STOP conditions

- Deleting a scope races a running job WITHOUT 036's leases (036 unlanded):
  destructive CLI must then require `--force-no-lease-check` and print a red
  warning; if you cannot make that guard airtight, STOP.
- `blocks()` unavailable on the target platform (macOS dev boxes) → gate
  physical bytes behind `cfg(target_os = "linux")` with logical-only fallback;
  if tests can't cover it, note and continue.
- The workspace has no flock crate and adding one conflicts with `deny.toml`
  advisories → STOP with the candidate list.

## Maintenance notes

- GC history (`gc-history.jsonl`) is the forensic trail the stability
  doctrine requires — never delete it in GC itself; rotate like trace.jsonl
  (`telemetry.rs:49` pattern).
- Budgets are initial guesses until the first estate campaign; revisit after
  plan 058's audit round.
