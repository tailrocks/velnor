# Plan 029 (design/spike): Shared-host cache GC + a `velnor-runner cache` inspect/GC subcommand

> **Executor instructions**: This is a **design/spike** plan — the deliverable is
> a design doc + a prototype behind a flag, not a finished feature. Investigate,
> prototype, define the policy, list open questions. STOP ⇒ report. Update
> `plans/README.md` when done.
>
> **Drift check (run first)**:
> `git diff --stat 1645ccc..HEAD -- crates/velnor-runner/src/runner.rs crates/velnor-runner/src/container.rs crates/velnor-runner/src/cli.rs`

## Status

- **Priority**: P2
- **Effort**: L (spike: M)
- **Risk**: MED
- **Depends on**: 006, 007 (GC should honor the trust/repo scoping those add)
- **Category**: direction
- **Planned at**: commit `1645ccc`, 2026-07-03

## Why this matters

The daemon mounts **unbounded, daemon-shared, persistent** host stores into every
job container — cargo registry/git, per-repo/workflow target buckets
(`_velnor_targets`), `_velnor_artifacts`, `_velnor_caches`, and a shared sccache
dir. There is **no eviction anywhere** (grep `evict|gc|prune|retention|LRU` over
the stores returns nothing). The only disk backstop is `disk_space_problem`
(`runner.rs:790`, `DISK_MIN_FREE_BYTES = 2 GiB`): below the floor a slot **parks**
(stops registering) and waits for a human. So caches grow until free space
crosses 2 GiB and **every slot parks → zero schedulable runners → jobs queue
forever** — the exact "healthy-looking daemon, no capacity" zombie class the
stability mandate says must self-heal, except here it waits instead of GC'ing.
This also blocks the 24h-zero-zombie soak gate. Operators of the always-on
Sentry host want cache visibility and reclamation now.

## Current state (evidence)

- Store roots (all under the daemon-shared root, unbounded):
  `container.rs:562-575` — `cargo_store_host` (`_velnor_cargo`),
  `mise_store_host` (`_velnor_mise`), `cargo_target_store_host` (`_velnor_targets`),
  `sccache_host` (`_velnor_sccache`); `executor.rs:3663` (`_velnor_artifacts`),
  `executor.rs:3673` (`_velnor_caches`).
- Disk backstop that only parks: `runner.rs:790-809` (`disk_space_problem`,
  2 GiB floor, sd_notify park status).
- Retention signal already threaded: `runtime_env.rs:91` carries
  `GITHUB_RETENTION_DAYS`.
- CLI has `doctor`/`status` (`cli.rs:14-30`) but no `cache` subcommand.
- sccache self-caps its own dir; the others do not.

## Deliverables

1. A short design doc `docs/cache-gc-design.md` covering:
   - Per-store retention policy: target buckets (`_velnor_targets`) → keep the N
     newest per `repo/workflow/job` bucket; `_velnor_caches`/`_velnor_artifacts`
     → honor `GITHUB_RETENTION_DAYS` + a byte ceiling (LRU by last-access/mtime);
     cargo registry/git → size-bounded LRU; sccache → already self-capped (leave).
   - Where GC runs: a background reaper in the daemon pre-flight loop, invoked
     **above** `disk_space_problem` so reclamation happens before parking.
   - Concurrency safety: GC must take the existing shared-store lock
     (`container.rs:59-62`) and must **never** delete a bucket an active job is
     using (skip in-use scopes).
   - The `velnor-runner cache` subcommand surface: `cache du` (report sizes per
     store/scope, read-only) and `cache gc [--dry-run] [--max-size ...]`.
2. A **prototype** behind a flag/env (`VELNOR_CACHE_GC=1`, default off): a
   read-only `cache du` and a `cache gc --dry-run` that reports what it *would*
   evict without deleting. Do not enable destructive GC by default in this spike.

## Commands you will need

| Purpose      | Command                                                          | Expected            |
|--------------|------------------------------------------------------------------|---------------------|
| Format       | `cargo fmt --all --check`                                        | exit 0              |
| Lint         | `cargo clippy --workspace --all-targets --locked -- -D warnings` | exit 0              |
| Subcommand   | `cargo run -p velnor-runner -- cache du`                        | prints store sizes  |
| Dry-run GC   | `cargo run -p velnor-runner -- cache gc --dry-run`             | lists candidates, deletes nothing |
| Tests        | `cargo nextest run -p velnor-runner cache --locked`            | pass                |

## Scope

**In scope (spike)**:
- `docs/cache-gc-design.md` (create).
- `crates/velnor-runner/src/cli.rs` + a new `cache` command handler — `du` and
  `gc --dry-run` only (read-only / non-destructive in this spike).
- Unit tests for the eviction-candidate selection (pure function over a listing).

**Out of scope (this spike)**:
- Actually deleting cache data by default — gate destructive GC behind a flag and
  do not enable it; a follow-up plan enables it after soak testing.
- Changing the parking backstop — leave `disk_space_problem` as the last resort.

## Git workflow

- Branch: `advisor/029-cache-gc-spike`
- Commit: `feat(cache): cache du + gc --dry-run spike; GC design doc`
- Do NOT push/PR unless instructed.

## Steps

### Step 1: Enumerate and size the stores

Write `cache du`: walk each store root (reuse the `*_host` path helpers) and
report total bytes per store and per scope (`<trust>/<repo>/...`). Read-only.

**Verify**: `cargo run -p velnor-runner -- cache du` prints sizes for each store.

### Step 2: Define + prototype eviction-candidate selection (dry-run)

Implement a pure function `select_eviction_candidates(listing, policy) -> Vec<PathBuf>`
per the design doc's policy, and wire `cache gc --dry-run` to print candidates
without deleting. Unit-test the selection (newest-N-kept, age/size thresholds,
in-use scope skipped).

**Verify**: `cargo run -p velnor-runner -- cache gc --dry-run` lists candidates
and deletes nothing; `cargo nextest run -p velnor-runner cache --locked` passes.

### Step 3: Write the design doc + open questions

Capture the policy, the reaper placement, the locking/in-use-skip rules, and open
questions (how to detect "in use"; interaction with plans 006/007 scoping; whether
to honor `GITHUB_RETENTION_DAYS` strictly). This doc is the spec a follow-up plan
implements destructive GC from.

**Verify**: `docs/cache-gc-design.md` exists and answers "what gets evicted, when,
and how is it made safe."

## Test plan

- Pure-function tests for eviction-candidate selection (keep-newest-N, age/size,
  in-use skip). No destructive test.
- Verification: `cargo nextest run --workspace --locked` → all pass.

## Done criteria

- [ ] `docs/cache-gc-design.md` defines per-store retention, reaper placement, and safety rules
- [ ] `velnor-runner cache du` reports store/scope sizes (read-only)
- [ ] `velnor-runner cache gc --dry-run` lists eviction candidates and deletes nothing
- [ ] Eviction-candidate selection is a tested pure function; destructive GC is behind a default-off flag
- [ ] `cargo fmt`/`clippy`/`nextest` all green
- [ ] `plans/README.md` row updated

## STOP conditions

- Determining "in-use scope" reliably is not possible with the current
  slot/job bookkeeping — document it as the key open question; do not ship
  destructive GC without it.
- The shared-store lock cannot be acquired from the daemon pre-flight context —
  report.

## Maintenance notes

- Destructive GC is a **follow-up** gated on this spike + a soak test; never
  enable it by default here.
- Ties to the stability mandate: GC-before-park converts a "park and wait for a
  human" failure into self-healing. Reviewer: scrutinize the in-use-skip and the
  lock usage — a wrong deletion mid-job corrupts a warm cache.
