# Plan 035: Canonical storage contract — /var/lib + /var/cache layout, fail-closed identity, catalog CLI

> **Executor instructions**: Follow step by step; run every verification; STOP
> conditions binding. Update the status row in `plans/README.md` when done.
>
> **Drift check (run first)**:
> `git diff --stat 48b04ad..HEAD -- crates/velnor-runner/src/container.rs crates/velnor-runner/src/cache.rs crates/velnor-runner/src/github_adapter.rs crates/velnor-runner/src/executor.rs`
> Re-locate excerpts by symbol on drift; semantic mismatch → STOP.

## Status

- **Priority**: P0 (V0.11; storage doc "Canonical on-disk contract")
- **Effort**: L
- **Risk**: HIGH (moves every persistent store; data migration)
- **Depends on**: none hard; plan 037 (GC) builds on this; land before 036
- **Category**: tech-debt / correctness
- **Planned at**: commit `48b04ad`, 2026-07-18

## Why this matters

`docs/storage-and-disk-pressure-2026-07-18.md` (READ sections "Decision",
"Canonical on-disk contract", "Catalog and accounting" FIRST — it is the
requirement source) is accepted P0 direction: one explicit layout
(`/var/lib` durable, `/var/cache` regenerable, `/run` lease, `/var/log`,
job-work), every store with identity/owner/budget/lifetime, and fail-closed
identity. Live Sentry evidence behind it: root XFS 84% used, ~432 GB in
persistent target trees whose paths collapse to
`unknown-repository/unknown-workflow` — a P0 correctness bug where unrelated
repos share a writable warm target dir. Today ALL stores live under
`<config>/_work/_velnor_*` and the unknown-fallback is silent.

## Current state (verified against `48b04ad`)

- Store roots: `crates/velnor-runner/src/container.rs:762-839` — constructors
  all `daemon_store_root(temp_host).join("_velnor_*")`: `sccache_host` (:763),
  `cargo_store_host` (:769), `mise_store_host` (:794),
  `cargo_target_store_host` (:819); `daemon_store_root` (:822-839) climbs
  `…/work/slot-N/<job>/temp` → `…/work`. `daemon_shared_root` (:743-760).
- GC/du view: `crates/velnor-runner/src/cache.rs:96-141` `store_roots()` —
  also covers `_velnor_caches` (:121), `_velnor_artifacts` (:127); sccache
  `gc_managed: false` (:138).
- Work root: `runner.rs:4711-4719` `job_work_dir` →
  `config_dir.join("_work")`; config dir `config.rs:46-57` →
  `$VELNOR_CONFIG_DIR` or `$HOME/.velnor/runner`.
- **P0 fail-open identity** (verified excerpt),
  `crates/velnor-runner/src/github_adapter.rs:70-86`:
  ```rust
  .join(sanitize_store_key(job_variable(job, "github.repository").unwrap_or("unknown-repository")))
  .join(sanitize_store_key(job_variable(job, "github.workflow").unwrap_or("unknown-workflow")))
  ```
  Same pattern: `executor.rs:4040-4057` (`cache_store_dir` →
  `_velnor_caches/<trust>/<repo>`), `container.rs:564-575`
  (`repository_store_key` for cargo/mise bin stores).
- No `/var/lib`/`/var/cache/velnor` code, no catalog, no `storage` CLI, no
  statvfs (grep-confirmed absent).
- Deb packaging exists for velnor-runner (`crates/velnor-runner/Cargo.toml:24`
  `[package.metadata.deb]`) — production installs are systemd services, so
  `/var/lib/velnor` + `/var/cache/velnor` are creatable via systemd
  directives or postinst; dev mode keeps `~/.velnor`-relative roots.

Target layout (from the storage doc — implement exactly):

```
/var/lib/velnor/v1/...              durable (runner identity/config)  — NOT moved by this plan beyond root resolution
/var/cache/velnor/v1/<trust>/cargo|mise|sccache|kache|targets|caches|artifacts|buildkit
/run/velnor/...                     leases (plan 036)
/var/log/velnor/...                 logs (existing log dirs keep working; alias only)
```

## Commands you will need

| Purpose | Command | Expected |
|---------|---------|----------|
| Gates | fmt / clippy / `cargo nextest run --workspace --locked` | exit 0 |
| Focused | `cargo nextest run -p velnor-runner storage` | new tests pass |

## Scope

**In scope**: `container.rs` (root resolution), `cache.rs` (store_roots),
`github_adapter.rs` + `executor.rs` (fail-closed identity), new
`crates/velnor-runner/src/storage.rs` (layout + catalog + resolution), `cli.rs`/`main.rs`
(`storage paths|status` subcommand), deb/systemd packaging entries for the new
dirs, `docs/storage-and-disk-pressure-2026-07-18.md` (status annotations).
**Out of scope**: capacity controller/leases/reclaim (plan 036), destructive
GC (plan 037), BuildKit builder creation (only the store PATH is reserved
here), physical-bytes accounting (037).

## Git workflow

Branch `velnor-estate-standard`; `feat(runner):` commits, `git commit -s`; no
push without operator instruction.

## Steps

### Step 1: `storage.rs` — one root-resolution authority

New module: `StorageLayout { cache_root, lib_root, run_root, log_root }` with
resolution order: explicit `--storage-root`/`VELNOR_STORAGE_ROOT` → system
mode (`/var/cache/velnor/v1` etc. when running as the packaged service —
detect via existing config-dir convention) → dev mode
(`<config>/_work`-relative, current behavior). EVERY store constructor in
`container.rs:762-839` and `cache.rs:96-141` re-routes through it; the
`_velnor_*` names become the layout's class names under
`<cache_root>/<trust>/<class>`. Legacy roots remain READABLE: resolution
checks the canonical path first, falls back to the legacy `_velnor_*` path if
it exists and the canonical does not (migration = operator `mv` per the
runbook note you add; no auto-migration in this plan).

**Verify**: tests `storage_layout_dev_mode_matches_legacy_roots`,
`storage_layout_system_mode_uses_var_cache`,
`legacy_store_readable_when_canonical_absent`. Existing store-path tests in
`container.rs:904+` updated, suite green.

### Step 2: Fail-closed identity (the P0 bug)

Replace the three `unwrap_or("unknown-*")` sites:
- `github_adapter.rs:70-86`: missing/empty `github.repository` or
  `github.workflow` → the persistent bucket is REFUSED — return an
  ephemeral job-private path under the job temp dir instead, and emit a
  `forensics.lifecycle` warning naming the missing variable (storage doc:
  "`unknown-*` permitted only for an ephemeral, job-private path").
  Additionally prefer a stable workflow identity: use
  `github.workflow_ref`-derived file path when available (check
  `job_variable(job, "github.workflow_ref")`) over the display name.
- `executor.rs:4040-4057` and `container.rs:564-575`: same rule — no
  persistent store keyed by `unknown-repository`.

**Verify**: tests `target_bucket_refuses_missing_repository_identity`
(asserts ephemeral path + no `_velnor_targets` write),
`cache_store_refuses_unknown_repository`; update the existing bucket-layout
test at `github_adapter.rs:852-867`. Suite green.

### Step 3: Catalog + `storage` CLI

`storage.rs` gains a catalog walk: enumerate every class/scope with logical
bytes (reuse `cache.rs` `dir_size` machinery), owner class
(durable/regenerable/lease/log/job-work), and budget placeholder (budgets
enforced in 036/037). New CLI `velnor-runner storage paths` (resolved layout,
mode, per-class existence) and `velnor-runner storage status` (catalog table).
Clap pattern: `Command::Cache` (`cli.rs:16,35-58`).

**Verify**: `cargo run -p velnor-runner -- storage paths` prints the resolved
layout in dev mode; `storage status` lists classes with byte counts; unit
test on the catalog builder with a tempdir tree.

### Step 4: Packaging

Deb/systemd: create `/var/cache/velnor` + `/var/lib/velnor` via systemd
`StateDirectory=`/`CacheDirectory=` (or postinst) with runner-user ownership.
Locate the unit files (deb assets, `crates/velnor-runner/Cargo.toml:42-45`)
and add the directives. Document the operator migration (`mv` legacy stores →
canonical; one paragraph in `docs/runner-usage.md`'s production section).

**Verify**: `cargo deb -p velnor-runner --no-build --no-strip` builds (or
STOP if cargo-deb absent locally — note it for the operator); unit file
contains the directives; runner-usage paragraph added.

### Step 5: Doc status

Annotate implemented items in the storage doc (`(closed: plan 035)` on the
canonical-contract + catalog lines; controller/GC lines stay open →
plans 036/037).

**Verify**: grep shows annotations only on implemented lines.

## Test plan

New tests per steps 1–3 (≥ 6). Pattern: existing store tests
`container.rs:904+`, `github_adapter.rs:852+`. Full gates green.

## Done criteria

- [ ] Gates exit 0; new tests pass; legacy-path fallback proven by test
- [ ] Zero `unwrap_or("unknown-` in store-path code
      (`grep -rn 'unknown-repository\|unknown-workflow' crates/velnor-runner/src/ | grep -v test` → only the refusal warning strings)
- [ ] `storage paths`/`status` functional
- [ ] No out-of-scope changes

## STOP conditions

- Existing daemons in the wild have populated `unknown-repository` buckets
  (they do — Sentry) and step 2 would strand ~432 GB: that is EXPECTED;
  reclamation is plan 037's, deletion is NOT this plan's job. If you find
  yourself writing deletion code, STOP.
- `github.workflow_ref` absent from real job messages (check the test
  fixtures in `runner.rs`/`github_adapter.rs` tests) → keep display-name
  identity, note it, continue.
- System-mode detection ambiguous (no clean signal for "running as packaged
  service") → default to explicit `VELNOR_STORAGE_ROOT` only + systemd unit
  setting it; do not invent heuristics. Note the decision.

## Maintenance notes

- Plans 036/037 assume `storage.rs` is the single path authority — any new
  store MUST register there (reviewers reject direct `join("_velnor_")`).
- The BuildKit store path is reserved but unowned until the controller plan;
  `docker buildx` state stays wherever Docker keeps it for now.
