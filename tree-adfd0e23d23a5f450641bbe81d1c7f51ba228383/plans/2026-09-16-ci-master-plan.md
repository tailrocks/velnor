# CI master plan — workflow structure + cache reuse

Status: **approved direction** (pre-implementation).  
Date: 2026-09-16.  
Repository: `tailrocks/velnor`.

This is the **single entry point**. Detailed analysis lives in the two companion documents:

| Document | Scope |
|---|---|
| [`2026-09-16-workflow-structure-restoration.md`](2026-09-16-workflow-structure-restoration.md) | Era A/B/C analysis, naming contract, aggregation model, generator WPs |
| [`2026-09-16-cache-reuse-and-retention-plan.md`](2026-09-16-cache-reuse-and-retention-plan.md) | Cache layers, gaps G1–G8, retention, event matrix |

---

## 1. Problems we are solving

### Workflow structure (UX)

- Era C (#867) hid crate identity behind lane-first groups (`GitHub / Rust / rust-velnor-runner`)
- Operators cannot see which crate is building at a glance
- 6377-line monolithic `ci-unit-rust.yml` with duplicated github+velnor bodies

### Cache reuse (performance)

- PRs must **restore** warm cache from main/nightly on every run
- Parallel PRs must **not** bloat Actions cache storage
- **merge_group** currently restores but never saves — gap after merge queue
- Workflow restoration must **not** invalidate cache keys or break Velnor prep ordering

---

## 2. Locked decisions

| # | Decision |
|---|---|
| D1 | **Unit-first check names:** `Rust · velnor-runner (GitHub)` / `(Velnor)` when `runners = both`; bare `Rust · velnor-runner` when single lane |
| D2 | **All kinds** use the same naming rule (Rust, Bun, Docker, Docs, OpenTofu) |
| D3 | Keep **`Control / Prepare Cargo`** — one shared Velnor Cargo warmup, not per crate |
| D4 | Keep **#867 both-lane contract** (pairing, fork admission, `lane_compare --strict`) |
| D5 | **Option 3 aggregation:** one aggregate caller per (unit, lane); kind reusable files kept (`ci-unit-*.yml`) |
| D6 | Accept **`Rust · velnor-runner (GitHub) / verify`** nesting in PR1; flat names optional in PR3 via composites |
| D7 | **Dependency-closure** edges move to aggregate `needs:` (not inner reusable `needs:`) |
| D8 | **PR read-only for cache saves** — PRs restore, never save trusted snapshots (fork safety + budget) |
| D9 | **Add merge_group to trusted save gate** — merge queue persists cache like main |
| D10 | **Cache keys unchanged** on restoration — keyed on `unit.id`, not display names |

---

## 3. Target end state

### Checks sidebar (runners = both, PR)

```
Control / Planning
Policy                          (main/nightly only)
Control / Velnor admission      (fork guard)
Rust · velnor-runner (GitHub)
Rust · velnor-runner (Velnor)
Rust · velnor-workflow (GitHub)
Rust · velnor-workflow (Velnor)
… (one row per unit × lane, all kinds)
Bun · velnor (GitHub)
Docker · Docker (Velnor)
Control / Prepare Cargo
Control / Aggregate
Control / Required
```

### Cache behavior (after all WPs)

| Event | Restore | Save trusted cache | Storage |
|---|---|---|---|
| PR (same-repo) | All layers | No | Read-only |
| PR (fork) | All layers | No | Read-only |
| merge_group | All layers | **Yes** (D9) | Bounded |
| push → main | All layers | Yes | Producer |
| nightly | All layers | Yes | Producer + maintenance |

---

## 4. Unified work packages

### PR1 — Structure + cache core (single merge)

| WP | Source | Tasks |
|---|---|---|
| **WP1** | workflow | `unit_job_display_name()`; per-(unit,lane) callers; drop lane-first kind triplets |
| **WP2** | workflow | `Control / Prepare Cargo`; Velnor Rust callers `needs` prep; admission unchanged |
| **WP3** | workflow | `lane_compare` parenthetical parser; rewrite `lane_pairing.rs`, `velnor_first_ci.rs` |
| **WP-C0** | cache | Golden cache-key tests; per-unit cache render; aggregate dependency `needs:` |
| **WP-C1** | cache | `trusted_cache_save_expression()` + **merge_group** in save gate |
| **WP-C3** | cache | Restore-always contract tests (generator must emit restore before checks) |
| **Regen** | both | `ci-pr`, `ci-main`, `nightly`, `ci-unit-*`, `release.yml` unit verify names |

**Files:** `crates/velnor-workflow/src/lib.rs`, `primitives/ir.rs`, `crates/velnor-tools/src/lane_compare.rs`, tests, `.github/workflows/*`

### PR1.1 or PR1 tail — mise alignment (if needed)

| WP | Source | Tasks |
|---|---|---|
| **WP-C2** | cache | Align `mise-action` with read-only PR policy |

### PR2 — Docs + visibility

| WP | Source | Tasks |
|---|---|---|
| **WP5** | workflow | `execution.mdx` hierarchy diagram; ruleset grep; changelog |
| **WP-C4** | cache | Cache hit/miss in step summary / `VELNOR_CI_REPORT` |
| **WP-C6** | cache | Verify maintenance schedule + budget alerts |

### PR3 — YAML size + optional flat names

| WP | Source | Tasks |
|---|---|---|
| **WP4** | workflow | Composite actions for shared steps; shrink `ci-unit-rust.yml` |
| **WP-C5** | cache | Narrow `rust-policy` mbx freshness paths (optional) |
| **Optional** | workflow | Inline aggregate jobs for zero-`/`-suffix check names |

---

## 5. Generator changes checklist (PR1)

### Naming and aggregation

- [ ] `unit_job_display_name(unit, lane, runners)` in `lib.rs`
- [ ] Replace `kind_reusable_callers()` with per-(unit,lane) in `render_node_callers()`
- [ ] Add `unit` + `lane` inputs to all kind reusables; one inner job `verify`, omit inner `name:`
- [ ] Caller job ids: `group-unit-{unit}-{lane}` (both) or `group-unit-{unit}` (single lane)
- [ ] `render_nodes_required()` lists ~37 caller ids (was ~14)

### Control plane

- [ ] Rename `Control / Rust` → `Control / Prepare Cargo` (`group-rust-prepare-cargo`)
- [ ] Velnor Rust unit callers `needs: [group-rust-prepare-cargo, …deps]`
- [ ] GitHub-lane Rust callers do not `needs` prep

### Dependency closure

- [ ] `aggregate_velnor_rust_needs()` resolves to aggregate caller ids
- [ ] Strip cross-unit `needs:` from inner reusables (single-job mode)
- [ ] Skipped-tolerance on prep + dependency callers in `if:` conditions

### Cache (must not break keys)

- [ ] Cache steps rendered from correct `inputs.unit` → `CacheSpec`
- [ ] Keys still use `unit.id` in mbx variant segment
- [ ] `trusted_cache_save_expression()` includes `merge_group`
- [ ] Golden test: identical keys before/after regen for fixed fixture

### Tooling and output

- [ ] `lane_compare` parses `(GitHub)` / `(Velnor)` suffix
- [ ] `release.rs`: `comparison_job_name` → `unit_job_display_name`
- [ ] Regenerate all workflows; `velnor-workflow --check` + policy + actionlint green

---

## 6. Success criteria (complete)

### Workflow structure

1. PR Checks list one row per unit × lane: `Rust · velnor-runner (GitHub)`, etc.
2. `lane_compare --strict` passes on both-lane `workflow_dispatch`
3. ≤50 unique reusable files with 30+ synthetic Rust crates
4. Shard test updated for per-unit callers on `ci-unit-rust-2.yml`
5. Fork PR: admission fails; GitHub-lane checks still run
6. `merge_group`: both lanes without admission job

### Cache reuse

7. PR with unchanged lockfile: cargo + rustup restore hit in logs
8. PR: no trusted save steps match `pull_request` in `if:`
9. merge_group: save steps include `merge_group` in `if:`
10. Two parallel PRs: cache entry count stable (no PR write growth)
11. PR close: merge-ref namespace pruned (`maintenance.yml`)
12. Daily maintenance: `total_bytes <= 8 GiB` budget
13. Golden test: cache keys unchanged for fixed fixture after restoration
14. Restore-always tests: no GitHub job runs checks before restore steps

---

## 7. Explicit non-goals

- Revert to lane-first kind callers (`GitHub / Rust`)
- Per-unit workflow **files** (50-file limit at scale)
- Save trusted cache from PR or fork PR
- Skip recompile when source files changed
- Dual-publish old and new check names
- Block PR1 on composite extraction (PR3)

---

## 8. Risk summary

| Risk | Mitigation |
|---|---|
| Restoration breaks cache keys | WP-C0 golden tests |
| Velnor cold/offline failures | Aggregate `needs:` + prep ordering |
| merge_group never warms cache | WP-C1 |
| Parallel PRs fill storage | PR read-only saves + merge-ref prune + retention |
| `/ verify` suffix in Checks | Accept PR1; PR3 composites optional |
| `ci-required` script size (~37 jobs) | Regenerate `render_nodes_required()`; test validation |

---

## 9. Document map

```
plans/2026-09-16-ci-master-plan.md          ← YOU ARE HERE (unified plan)
plans/2026-09-16-workflow-structure-restoration.md   (detailed workflow analysis)
plans/2026-09-16-cache-reuse-and-retention-plan.md   (detailed cache analysis)
fleet/ci-cd-cache-architecture-evidence.md           (live evidence, Sep 2025)
```

---

## 10. Next step

Implement **PR1** per §4 and §5. One merge containing workflow restoration + cache WP-C0/C1/C3 + regen.
