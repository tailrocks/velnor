# CI workflow restoration and dual-lane cache plan

Status: **approved direction** — goal-ready execution checklist.  
Date: 2026-09-16 (updated: dual-lane cache policy).  
Repository: `tailrocks/velnor`.  
Purpose: Single source for `/goal` — workflow structure + **separate GitHub and Velnor cache policies**.

Related: PR [#867](https://github.com/tailrocks/velnor/pull/867).

---

## 0. Goal statement

Deliver CI where:

1. **Checks are unit-first** — `Rust · velnor-runner (GitHub)`, not `GitHub / Rust / rust-velnor-runner`.
2. **Velnor lane never loses store warmth** — host-persistent cargo/mise/mbx with **50 GiB** budget; structural guarantee, not GHA key luck.
3. **GitHub lane maximizes restore within platform limits** — 8 GiB internal budget below 10 GB platform cap; strict producer/consumer; prefix restore as normal warm path.
4. **Outcomes are predictable** — observability distinguishes exact hit, prefix restore, host-warm, and cold per lane.

---

## 1. Dual-lane cache philosophy

Velnor exists because **GitHub Actions cache is small, shared, and easy to evict**. The Velnor lane uses **host disk with bind mounts** — a different storage system entirely.

```
┌─────────────────────────────────────────────────────────────────────────┐
│  SAME cache keys per unit (unit.id, hashFiles) — lane-agnostic keys   │
│  DIFFERENT transport + budget + retention per lane                      │
└─────────────────────────────────────────────────────────────────────────┘

  GitHub lane                          Velnor lane
  ───────────                          ───────────
  Transport: GHA cache API             Transport: host bind mounts
  Budget: 8 GiB (internal)             Budget: 50 GiB (host)
  Platform cap: 10 GB                  Platform cap: host disk
  Enforcer: maintenance.yml            Enforcer: velnorctl cache gc
  mbx: backend github, objects         mbx: backend local
  PR: restore-only                     PR: writes to host store (trust-scoped)
  "Never miss": NOT achievable         "Never miss stores": achievable
```

**Do not conflate the two budgets.** Raising Velnor to 50 GiB does **not** mean raising `RetentionPolicy.total_bytes` in `snapshot.rs` — that policy governs **GitHub Actions cache API only**.

---

## 2. Platform and budget reference

### 2.1 GitHub Actions cache (GitHub lane only)

| Fact | Value | Velnor policy |
|---|---|---|
| Default repo storage | **10 GB** | Internal budget **8 GiB** (~1.3 GiB headroom) |
| Inactivity eviction | 7 days (`last_accessed_at`) | Cannot control; producers must refresh |
| Over-limit eviction | Platform LRU | Why we stay below 10 GB internally |
| Per-entry max | ~10 GiB | Class budgets stay under this |
| PR writes | `refs/pull/N/merge` namespace only | velnor also blocks trusted saves on PR |
| Fork PR | Cannot write default-branch scope | Restore only — platform rule |
| Key max length | 512 chars | mbx hashFiles lists must fit |

**Source:** `crates/velnor-workflow/src/primitives/snapshot.rs` → `RetentionPolicy::default_policy()`  
**Enforcement:** `.github/workflows/maintenance.yml` → `cache-budget` daily 03:31 UTC

#### GitHub internal class table (8 GiB — keep)

| Class | Budget | Tier | Generation bound | Key markers |
|---|---:|---|---:|---|
| toolchain-seeds | 2 GiB | Protected | 0 | `velnor-rustup-`, `velnor-mold-` |
| source-bundles | 1.5 GiB | Protected | 0 | `ci-*-rust-*`, `velnor-cargo-*` |
| docker-seed | 1 GiB | Baseline | 1 | `velnor-docker-seed-` |
| compiler-snapshots | 3.5 GiB | Rolling | **2** | `-mbx-v3-` |
| **Total** | **8 GiB** | | | |

- Producer window: 2 hours
- Eviction: generation bound → class budget → global; protected never global-swept
- PR cleanup: `prune-pr-cache` on PR close

### 2.2 Velnor host storage (Velnor lane only)

| Store class | Host path | Container mount | Default budget env |
|---|---|---|---|
| cargo registry/git | `/var/cache/velnor/v1/.../cargo/` | `~/.cargo/registry`, `~/.cargo/git` | `VELNOR_BUDGET_CARGO_BYTES` = 20 GiB |
| mise | `.../mise/` | `/opt/mise/cache`, `/opt/mise/installs` | `VELNOR_BUDGET_MISE_BYTES` = 20 GiB |
| mbx compiler | `.../compiler/mbx/` | `/var/cache/mbx` | `MBX_GC_MAX_TOTAL_SIZE` = **50 GiB** |
| targets | `.../targets/` | mbx-managed | `VELNOR_BUDGET_TARGETS_BYTES` = 200 GiB |
| actions-cache class | `.../caches/` | local tarball store | `VELNOR_BUDGET_CACHES_BYTES` = **50 GiB** |
| hosted GHA emulator | `.../gha-cache/` | optional | `VELNOR_GHA_CACHE_BUDGET_BYTES` = 10 GiB |

**Defaults already at 50 GiB** for caches class and mbx total:

```407:412:crates/velnorctl/src/runtime.rs
    #[arg(
        long,
        env = "VELNOR_BUDGET_CACHES_BYTES",
        default_value_t = 53_687_091_200u64
    )]
    pub budget_caches_bytes: u64,
```

```470:470:crates/velnor-runner/src/container.rs
                ("MBX_GC_MAX_TOTAL_SIZE", "50GiB"),
```

**Enforcement:** `velnorctl cache gc` on fleet hosts (operator-scheduled); disk-pressure reclaim at 2 GiB free floor.

**Velnor lane CI behavior today (correct — preserve):**

- `lane_enables_actions_cache` → **false** when all paths host-persistent (`cache.rs`)
- mbx `backend: local` — no GHA I/O (`ir.rs`)
- `Control / Prepare Cargo` warms shared cargo before parallel Rust jobs
- Log: *"Cache paths live on Velnor host-persistent storage (always warm)"*

---

## 3. What "never miss cache" means per lane

### Velnor lane — store layers (~100% achievable)

| Layer | Target | Mechanism |
|---|---|---|
| cargo registry/git | **Always warm** after first populate | Host bind mounts survive jobs |
| mise tools | **Always warm** | Host mounts + explicit install |
| mbx objects | **Hit for unchanged source** | `backend: local`; WP6 evidence: 1651 hits, 0 B transfer |
| mbx on source edit | Recompile, not store miss | Incremental compile — correct |
| First run / new host | One-time cold | Acceptable |
| Host GC at 50 GiB | Eviction possible | Monitor + schedule gc |

**Velnor does not use GHA cache for host-persistent paths.** "Never miss" = mounts + prep + offline probe, not `cache-hit=true`.

### GitHub lane — best-effort within platform (NOT 100%)

| Scenario | Expected | Fix |
|---|---|---|
| PR, unchanged lockfile | rustup/cargo exact hit | Keep producers healthy |
| PR, `.rs` change | mbx exact miss, **prefix restore** | By design; fix observability |
| PR | Never saves trusted scope | By design (D7) |
| Fork PR | Often cold for main keys | Platform limit — document |
| Platform LRU / 7-day idle | Miss after eviction | main + nightly producers |
| Compat digest rotation | Cold until main re-saves | Ensure main saves |
| merge_group without save | Miss | WP-C1 |
| mise PR writes | Budget pressure | WP-C2 |

**Realistic GitHub target:** ≥95% warm on same-repo PR with stable lockfile; prefix mbx on source edits; cold only on toolchain/schema shifts or eviction.

---

## 4. Root causes — why GitHub lane "always misses"

| ID | Root cause | Lane | Fix |
|---|---|---|---|
| RC-1 | PR restore-only consumer | GitHub | Keep; ensure producers |
| RC-2 | Observability: prefix restore reports as miss | GitHub | Phase 5 |
| RC-3 | `merge_group` never saves | GitHub | WP-C1 |
| RC-4 | `trusted_cache` duplicated 6+ places | GitHub | WP-C1 |
| RC-5 | mise writes on PR | GitHub | WP-C2 |
| RC-6 | `rust-policy` freshness = `./**/*.rs` | Both keys | WP-C5 |
| RC-7 | Live GHA account exceeded 8 GiB | GitHub | Phase 4 maintenance |
| RC-8 | Plan conflated 8 GiB with Velnor | Docs | **Fixed in this plan** |
| RC-9 | No scheduled `velnorctl cache gc` on fleet | Velnor | Phase 6 ops |
| RC-10 | `cache-plan --check` not implemented | GitHub | Phase 4 |

Velnor lane misses on GitHub account are **not applicable** — Velnor bypasses GHA for host-persistent paths.

---

## 5. Locked decisions

| # | Decision |
|---|---|
| D1 | Unit-first check names with `(GitHub)`/`(Velnor)` suffix when `runners = both` |
| D2 | Same naming rule for all unit kinds |
| D3 | Keep **`Control / Prepare Cargo`** — Velnor-only shared warmup |
| D4 | Keep #867 both-lane contract |
| D5 | One aggregate caller per (unit, lane); kind reusable files kept |
| D6 | Dependency-closure on aggregate `needs:` |
| D7 | **GitHub lane:** PR read-only for trusted GHA saves |
| D8 | **GitHub lane:** add `merge_group` to trusted save gate |
| D9 | Cache keys keyed on `unit.id` — same keys both lanes; restoration must not change keys |
| D10 | **GitHub lane budget: 8 GiB** internal (below 10 GB platform) — do not raise without measured overrun |
| D11 | **Velnor lane budget: 50 GiB** host (`VELNOR_BUDGET_CACHES_BYTES`, `MBX_GC_MAX_TOTAL_SIZE`) |
| D12 | **Separate retention systems** — never merge GHA `RetentionPolicy` with Velnor host GC |
| D13 | Velnor lane: no GHA save steps for host-persistent paths |
| D14 | Observability: exact / prefix / host-warm / cold — never fail CI on miss |

---

## 6. Target configuration (`.github-gen/velnor-workflow.toml`)

```toml
# Proposed — wire in Phase 6 schema work
[cache.github]
budget_bytes = 8589934592              # 8 GiB — GHA account only
producer_window_seconds = 7200
mbx_generation_bound = 2

[cache.velnor]
budget_bytes = 53687091200             # 50 GiB — host store only
producer_window_seconds = 86400        # longer protection on host
mbx_generation_bound = 6               # more generations than GitHub (2)

# Lane-agnostic — keep
[[declare]]
primitive = "cache-contract"
# backend = "detected" (default)
```

Until schema lands, Velnor 50 GiB is enforced via **fleet env** (`VELNOR_BUDGET_*`, `MBX_GC_MAX_TOTAL_SIZE`).

---

## 7. GitHub lane stack (optimize within restrictions)

### Layers

| Layer | Key | Restore | Save gate | Budget class |
|---|---|---|---|---|
| rustup | `velnor-rustup-{os}-{arch}-{hashFiles(toolchain)}` | Always | Trusted only | toolchain-seeds 2 GiB |
| mold | `velnor-mold-2.42.0-{os}-{arch}` | Always | Trusted only | toolchain-seeds |
| cargo | `ci-{os}-rust-{hashFiles(key_files)}` + prefix `ci-{os}-rust-` | Always | Trusted + miss | source-bundles 1.5 GiB |
| mbx | `velnor-mbx-v3-{compat}-{os}-{arch}-{unit.id}-{dep}-{fresh}` + 2-tier prefix | Always | mbx action + trusted | compiler-snapshots 3.5 GiB, gen≤2 |
| docker seed | `velnor-docker-seed-v3-…` | Always on PR | Trusted + export | docker-seed 1 GiB, gen≤1 |
| mise | action-managed | Always | **Must gate off on PR** | unbudgeted — fix WP-C2 |

### Trusted save gate (target — single helper)

```
merge_group
|| (push && ref == refs/heads/main)
|| schedule
|| (workflow_dispatch && ref == refs/heads/main)
```

**Excludes:** all `pull_request` variants.

### Producer schedule

```
nightly 03:17 UTC  →  warm producers
maintenance 03:31  →  enforce 8 GiB budget
main push          →  primary producer
```

---

## 8. Velnor lane stack (50 GiB — maximize certainty)

### Layers

| Layer | Mechanism | GHA I/O |
|---|---|---|
| cargo registry/git | Host bind mounts | **None** |
| mise | Host mounts + `mise install` | **None** |
| mbx | `backend: local` | **None** |
| rustup/mold | Image-baked | **None** |
| Docker | Retained BuildKit on host | **None** (no seed transport) |
| workspace | Per-job UUID checkout | N/A (not cached) |

### Host budget allocation (50 GiB target)

| Class | Proposed share | Env / mechanism |
|---|---:|---|
| mbx compiler stores | 30 GiB | `MBX_GC_MAX_TOTAL_SIZE=50GiB` (total cap); tune per-class in gc |
| cargo + mise combined | 8 GiB | `VELNOR_BUDGET_CARGO_BYTES`, `VELNOR_BUDGET_MISE_BYTES` |
| docker/buildkit accounted | 8 GiB | `HostCapacity` — docker subtracted from promisable |
| headroom / artifacts overlap | 4 GiB | operator buffer |

### Operational requirements

- [ ] `velnorctl cache gc` scheduled on fleet (daily or weekly)
- [ ] `velnorctl cache du` + `velnorctl storage status` in runbook
- [ ] Alert when host available < 5 GiB
- [ ] Do **not** enable `VELNOR_ACTIONS_CACHE_URL` unless non-persistent paths need it

---

## 9. Execution checklist — Phase 1: Generator core (PR1)

### 9.1 Workflow structure (WP1–WP4)

- [ ] Add `unit_job_display_name(unit, lane, runners)` in `lib.rs`
- [ ] Replace `kind_reusable_callers()` with per-(unit,lane) callers
- [ ] Add `unit` + `lane` inputs to all kind reusables; one job `verify`, omit inner `name:`
- [ ] Regenerate `render_nodes_required()` for ~37 caller ids
- [ ] Rename `Control / Rust` → `Control / Prepare Cargo`
- [ ] Lift `velnor_rust_dependency_needs()` to aggregate `needs:`
- [ ] Update `lane_compare`, rewrite tests, regen workflows

### 9.2 GitHub lane cache fixes (WP-C0–C3)

- [ ] Add `trusted_cache_save_expression(default_branch)` in `ir.rs`
- [ ] Replace all 6+ inline `trusted_cache` literals (cargo, mold, rustup, docker, release)
- [ ] Include `merge_group` in save gate; exclude `pull_request`
- [ ] Verify mr-boxington saves on `merge_group`; bump pin if needed
- [ ] Golden test: cache keys unchanged for fixed fixture
- [ ] Test: restore always before checks on GitHub lane
- [ ] Test: Velnor lane skips `actions/cache` when host-persistent
- [ ] Test: Save `if:` includes `merge_group`, excludes `pull_request`
- [ ] Gate mise `cache: true` on trusted events only (WP-C2)
- [ ] Audit producer `actions: write` / `cache-mode` on main/nightly if saves fail

### 9.3 Velnor lane cache preservation (WP-V0)

- [ ] Confirm `lane_enables_actions_cache` returns false for Velnor + host-persistent paths
- [ ] Confirm mbx renders `backend: local` on Velnor lane (no GHA key transport)
- [ ] Confirm `Control / Prepare Cargo` runs before Velnor Rust wave in both mode
- [ ] Confirm Velnor jobs use offline cargo probe (not `cache-hit` gate)
- [ ] Confirm Docker Velnor lane uses retained builder, no seed restore/save
- [ ] Test: Velnor lane YAML has zero `actions/cache/save` for host-persistent units

### 9.4 Phase 1 gate

- [ ] `velnor-workflow --check` + policy + actionlint green
- [ ] §16 verification Phase 1 items pass

---

## 10. Execution checklist — Phase 2: Generated output audit

- [ ] GitHub jobs: full restore stack (rustup → mbx → mold → cargo → checks → save gated)
- [ ] Velnor jobs: no GHA cargo restore/save; mbx local; prep ordering correct
- [ ] All Save steps use `trusted_cache_save_expression` (no inline drift)
- [ ] `maintenance.yml`: schedule 03:31, `actions: write`, enforce 8 GiB
- [ ] `nightly.yml`: schedule 03:17 (before maintenance)

---

## 11. Execution checklist — Phase 3: Key churn (PR3)

- [ ] Narrow `rust-policy` mbx freshness — drop `./**/*.rs`; use deny/audit configs
- [ ] Add `deny.toml` / `audit.toml` to policy dep segment
- [ ] Composite extraction; restore/save gates stay in one job
- [ ] Re-measure shard budget

---

## 12. Execution checklist — Phase 4: GitHub budget ops

- [ ] Confirm org GHA cache limit (default 10 GB)
- [ ] Keep `RetentionPolicy.total_bytes = 8589934592` unless org limit lower
- [ ] Implement `cache-plan --check` OR remove from docs
- [ ] Prove 7 consecutive daily `cache-budget` runs ≤ 8 GiB
- [ ] Headroom warning when `headroom_bytes < 512 MiB`
- [ ] Consider hard-fail `prune-pr-cache` on DELETE failure
- [ ] merge_group: enable queue + save gate OR remove dead trigger

---

## 13. Execution checklist — Phase 5: Observability (PR2)

- [ ] `VELNOR_CI_REPORT.cache_outcomes.{rustup,mold,cargo,mbx}` = `exact|prefix|cold`
- [ ] Velnor lane reports `host_warm` for bypassed GHA layers
- [ ] Step summary per unit job; warn on cold, never fail CI
- [ ] Update `content/docs/guides/execution.mdx` — dual-lane diagram
- [ ] Document GitHub producer/consumer vs Velnor host-persistent model
- [ ] Document fork PR and platform 7-day eviction limits

---

## 14. Execution checklist — Phase 6: Velnor host ops (50 GiB)

### Fleet configuration

- [ ] Set `VELNOR_BUDGET_CACHES_BYTES=53687091200` on fleet hosts (default already)
- [ ] Confirm `MBX_GC_MAX_TOTAL_SIZE=50GiB` in job containers (default already)
- [ ] Tune `VELNOR_BUDGET_CARGO_BYTES` / `VELNOR_BUDGET_MISE_BYTES` to disk capacity
- [ ] Schedule `velnorctl cache gc --yes` (cron or systemd timer on sentry)
- [ ] Run `velnorctl cache du --work-dir /var/lib/velnor/work` after gc; record baseline

### Schema wiring (generator)

- [ ] Add `[cache.github]` and `[cache.velnor]` to `RepoGenerationConfig` (`config/mod.rs`)
- [ ] `RetentionPolicy::from_config(&cache.github)` for `cache-plan`
- [ ] Emit Velnor host policy artifact (JSON or `velnor.env` snippet) from generator
- [ ] Golden test: adding `[cache.*]` does not change cache keys
- [ ] Document Velnor policy in operator docs (`storage-and-resources.mdx`)

### Monitoring

- [ ] Alert when host disk available < 5 GiB
- [ ] Track mbx hits/misses in phase reports (WP6 evidence format)
- [ ] Prove consecutive Velnor jobs: mbx hits > 0, 0 B GHA transfer
- [ ] Prove `cache du` total ≤ 50 GiB after sustained nightly load

---

## 15. What not to do

### Workflow

- [ ] Do not revert to lane-first kind callers
- [ ] Do not revert to per-unit workflow files
- [ ] Do not hand-edit `.github/workflows/*`

### Cache — GitHub lane

- [ ] Do not save trusted GHA snapshots from PR or fork PR
- [ ] Do not add `pull_request` to trusted save gate
- [ ] Do not raise GHA `RetentionPolicy` above 8 GiB without measured overrun + org limit check
- [ ] Do not use `cache-matched-key` to skip `cargo fetch` (prefix may be stale lockfile)
- [ ] Do not fail CI on cache miss

### Cache — Velnor lane

- [ ] Do not add GHA `actions/cache` save steps for host-persistent paths
- [ ] Do not lower Velnor host budget below 50 GiB without disk constraint proof
- [ ] Do not merge Velnor host stores into GHA `RetentionPolicy` / `maintenance.yml`
- [ ] Do not enable hosted GHA cache emulator unless non-persistent paths require it

### Cache — both lanes

- [ ] Do not put display names in cache keys
- [ ] Do not skip recompile when source changed
- [ ] Do not split restore/save gates across composite jobs

---

## 16. Verification gates

### Phase 1 — PR1 merge

#### Workflow

- [ ] Unit-first Checks sidebar; `lane_compare --strict` green
- [ ] ≤50 reusable files; shard test passes
- [ ] Fork admission + merge_group behavior correct

#### GitHub lane cache

- [ ] Golden keys unchanged
- [ ] Restore before checks (automated)
- [ ] PR: no save on `pull_request`; merge_group in save gate
- [ ] Same-repo PR + stable lockfile: rustup/cargo exact hit
- [ ] Parallel PRs: GHA entry count stable

#### Velnor lane cache

- [ ] Zero GHA cache I/O for host-persistent cargo/mbx
- [ ] `Control / Prepare Cargo` ordering preserved
- [ ] Consecutive Velnor jobs: cargo offline probe passes without fetch
- [ ] mbx local hits > 0 on unchanged source (log evidence)

### Phase 4 — GitHub budget

- [ ] 7 daily maintenance runs: `total_bytes <= 8589934592`
- [ ] `failed_evictions == 0`

### Phase 6 — Velnor host

- [ ] `velnorctl cache du` ≤ 50 GiB after load
- [ ] Scheduled gc runs green
- [ ] Host disk headroom ≥ 5 GiB under normal load

### Manual smoke

- [ ] PR GitHub: rustup/cargo hit on doc-only change
- [ ] PR GitHub: mbx prefix restore on single-crate `.rs` change
- [ ] PR Velnor: log shows host-persistent warm, no fetch
- [ ] main push: GitHub save steps run

---

## 17. Files to change

| Area | Path | Phase |
|---|---|---|
| Dual-lane config schema | `crates/velnor-workflow/src/config/mod.rs` | 6 |
| GitHub retention | `crates/velnor-workflow/src/primitives/snapshot.rs` | 4, 6 |
| Cache gates, lane render | `crates/velnor-workflow/src/primitives/ir.rs` | 1 |
| Host-persistent bypass | `crates/velnor-workflow/src/primitives/cache.rs` | 1 (verify) |
| cache-plan CLI | `crates/velnor-workflow/src/runtime.rs` | 4, 6 |
| Maintenance template | `crates/velnor-workflow/src/primitives/release.rs` | 4 |
| Host GC defaults | `crates/velnorctl/src/runtime.rs` | 6 (verify) |
| mbx container limits | `crates/velnor-runner/src/container.rs` | 6 (verify) |
| Operator docs | `content/docs/operations/storage-and-resources.mdx` | 5, 6 |
| CI execution docs | `content/docs/guides/execution.mdx` | 5 |
| Config | `.github-gen/velnor-workflow.toml` | 6 |
| Generated workflows | `.github/workflows/*` | 1 (regen) |

---

## 18. Delivery phases for `/goal`

| Phase | Delivers | Gate |
|---|---|---|
| **1** | Workflow structure + GitHub cache fixes + Velnor bypass verified + regen | §16 Phase 1 |
| **2** | Generated YAML audit (GitHub vs Velnor stacks) | §10 |
| **3** | Key churn reduction (policy freshness, composites) | §11 |
| **4** | GitHub 8 GiB budget proof + maintenance hardening | §16 Phase 4 |
| **5** | Dual-lane observability + operator docs | §13, §16 |
| **6** | Velnor 50 GiB fleet ops + config schema wiring | §14, §16 Phase 6 |

**Start Phase 1.** GitHub and Velnor cache work proceed in parallel within Phase 1 — same keys, different transport verification.

---

## 19. Architecture reference

### Cache key formats (lane-agnostic — must not change on restoration)

```
mbx:    velnor-mbx-v3-{12-hex-compat}-${{ runner.os }}-${{ runner.arch }}-{unit.id}-{dep-hashFiles}-{freshness-hashFiles}
cargo:  ci-${{ runner.os }}-rust-${{ hashFiles(unit.cache.key_files) }}
rustup: velnor-rustup-${{ runner.os }}-${{ runner.arch }}-${{ hashFiles('rust-toolchain.toml', 'rust-toolchain') }}
mold:   velnor-mold-2.42.0-${{ runner.os }}-${{ runner.arch }}
docker: velnor-docker-seed-v3-{digest}-…-docker-{compat-hashFiles}-{context-hashFiles}
```

### Scale (runners = both)

| Metric | Era C (now) | After Phase 1 |
|---|---:|---:|
| Verification units | 17 | 17 |
| Top-level aggregate jobs | 15–16 | ~39 |
| Unique reusable files | 5 | 5–6 |
| GitHub GHA budget | 8 GiB | 8 GiB |
| Velnor host budget | 50 GiB (defaults exist) | 50 GiB (documented + gc scheduled) |

### Risks

| Risk | Mitigation |
|---|---|
| Conflating GHA 8 GiB with Velnor 50 GiB | Dual-lane sections in this plan (§1, §2) |
| GitHub restoration breaks keys | WP-C0 golden tests |
| Velnor cold/offline | Prep ordering + host mounts |
| GitHub merge_group never warms | WP-C1 |
| GitHub budget overrun | Daily maintenance + 8 GiB enforce |
| Velnor disk full | 50 GiB gc + headroom alerts |
| False "always miss" on GitHub | exact/prefix/cold observability |
| False "miss" on Velnor | Report `host_warm` not GHA cache-hit |
