# CI workflow restoration and cache reuse plan

Status: **approved direction** (pre-implementation).  
Date: 2026-09-16.  
Repository: `tailrocks/velnor`.  
Related: PR [#867](https://github.com/tailrocks/velnor/pull/867), [`fleet/ci-cd-cache-architecture-evidence.md`](../fleet/ci-cd-cache-architecture-evidence.md).

---

## 1. Why we are doing this

**Workflow structure:** Era C (#867) made lane visible but hid crates behind lane-first groups (`GitHub / Rust / rust-velnor-runner`). Operators cannot see which crate is building at a glance.

**Cache reuse:** PRs must restore warm cache from main/nightly on every run. Parallel PRs must not bloat Actions storage. `merge_group` currently restores but never saves. Restoration must not invalidate cache keys or break Velnor prep ordering.

**Verdict:** Implement **Option 3** — unit-first flat names + kind reusable files + parenthetical lane suffix when both lanes + shared `Control / Prepare Cargo`. Keep #867 both-lane contract (pairing, fork admission, `lane_compare --strict`).

---

## 2. Target end state

### Checks sidebar (`runners = both`, PR)

```
Control / Planning
Policy                          (main/nightly only)
Control / Velnor admission      (fork guard)
Rust · velnor-runner (GitHub)
Rust · velnor-runner (Velnor)
Rust · velnor-workflow (GitHub)
… (one row per unit × lane, all kinds)
Bun · velnor (GitHub)
Docker · Docker (Velnor)
Control / Prepare Cargo
Control / Aggregate
Control / Required
```

When `runners = github` or `runners = velnor` only: bare names (`Rust · velnor-runner`) with no `(GitHub)` / `(Velnor)` suffix.

Acceptable PR1 check nesting: `Rust · velnor-runner (GitHub) / verify` (GitHub composes caller + inner job). Truly flat names (no `/`) are optional PR3 via composite actions.

### Cache behavior (after all work)

| Event | Restore | Save trusted cache | Storage |
|---|---|---|---|
| PR (same-repo) | All layers | No | Read-only |
| PR (fork) | All layers | No | Read-only |
| merge_group | All layers | Yes | Bounded |
| push → main | All layers | Yes | Producer |
| nightly | All layers | Yes | Producer + maintenance |

### Locked decisions

| # | Decision |
|---|---|
| D1 | Unit-first names: `Rust · velnor-runner (GitHub)` / `(Velnor)` when `runners = both`; bare name when single lane |
| D2 | Same naming rule for Rust, Bun, Docker, Docs, OpenTofu |
| D3 | Keep **`Control / Prepare Cargo`** — one shared Velnor Cargo warmup, not per crate |
| D4 | Keep #867 both-lane contract |
| D5 | One aggregate caller per (unit, lane); kind reusable files kept (`ci-unit-*.yml`) |
| D6 | Dependency-closure edges on aggregate `needs:`, not inner reusable `needs:` |
| D7 | PR read-only for cache saves (fork safety + budget) |
| D8 | Add `merge_group` to trusted save gate |
| D9 | Cache keys unchanged on restoration — keyed on `unit.id`, not display names |

---

## 3. What to do

### PR1 — Structure + cache core (single merge)

Ship workflow restoration and cache fixes together.

#### WP1 — Naming and aggregation

- [ ] Add `unit_job_display_name(unit, lane, runners)` in `lib.rs` — parenthetical suffix only when `runners = both`
- [ ] Replace `kind_reusable_callers()` with per-(unit,lane) callers in `render_node_callers()`
- [ ] Set caller `name` = `unit_job_display_name(...)`
- [ ] Add `unit` + `lane` inputs to all kind reusables; each invocation renders **one** job keyed `verify`, **omit inner `name:`**
- [ ] Caller job ids: `group-unit-{unit}-{lane}` (both) or `group-unit-{unit}` (single lane)
- [ ] Regenerate `render_nodes_required()` for ~37 caller ids (was ~14)

#### WP2 — Control plane

- [ ] Keep `Control / Velnor admission`, `Control / Aggregate`, `Control / Required`
- [ ] Rename `Control / Rust` → **`Control / Prepare Cargo`** (`group-rust-prepare-cargo`)
- [ ] Velnor Rust unit callers `needs: [group-rust-prepare-cargo, …deps]`; GitHub-lane Rust callers do not

#### WP3 — Dependency closure

- [ ] Lift `velnor_rust_dependency_needs()` to aggregate caller ids (e.g. `Rust · velnor-client (Velnor)` waits for `Rust · velnor-model (Velnor)` at aggregate level)
- [ ] Strip cross-unit `needs:` from inner reusables (single-job mode)
- [ ] Skipped-tolerance on prep + dependency callers in `if:` conditions

#### WP4 — Contract tooling

- [ ] Update `lane_compare` regex for `(GitHub)` / `(Velnor)` suffix when both; bare name when single lane
- [ ] Rewrite `lane_pairing.rs`, `velnor_first_ci.rs`, `synthetic_surface.rs`, shard tests
- [ ] `release.rs`: `comparison_job_name` → `unit_job_display_name`
- [ ] Regenerate `.github/workflows/*`

#### WP-C0 — Cache-safe restoration (no new cache behavior)

- [ ] Render cache steps from `inputs.unit` → correct `CacheSpec`
- [ ] Never put display names or caller ids in cache keys
- [ ] Golden test: regen unchanged → identical cache keys + `hashFiles(...)` args per unit

#### WP-C1 — Trusted save gate + merge_group

- [ ] Add `trusted_cache_save_expression(default_branch)` in `ir.rs`
- [ ] Replace all inline trusted-cache literals (cargo, mold, rustup, Docker seed, release paths)
- [ ] Include `merge_group` in save gate
- [ ] Test: generated YAML save steps include `merge_group` in `if:`

#### WP-C3 — Restore-always contract tests

- [ ] Every GitHub-lane job has restore steps (or documented Velnor host-persistent bypass)
- [ ] No GitHub Rust job runs checks before restore steps
- [ ] `Prepare Cargo sources` gated `if: steps.cache.outputs.cache-hit != 'true'`

#### PR1 verification gate

- [ ] `velnor-workflow --check` clean
- [ ] `ci-policy.yml` green
- [ ] `actionlint` green
- [ ] All items in **§6 Verification checklist** for PR1 pass

---

### PR1 tail — mise alignment (if needed)

#### WP-C2 — Align mise-action with read-only PR policy

- [ ] Gate mise saves with same `trusted_cache_save_expression`, or set `cache: false` on PR jobs
- [ ] After 10 same-repo PR runs without main push, cache entry count does not grow from mise-only keys

---

### PR2 — Docs + observability

#### WP5 — Operator docs

- [ ] Update `content/docs/guides/execution.mdx` with hierarchy diagram
- [ ] Grep org rulesets / dashboards for stale check names (`GitHub / Rust / …`)
- [ ] Changelog for check name rename

#### WP-C4 — Cache hit visibility

- [ ] Emit structured `cache_hit: {rustup,mold,cargo,mbx}` in `VELNOR_CI_REPORT` / step summary
- [ ] Warn on miss; do **not** fail CI on miss

#### WP-C6 — Retention verification

- [ ] Confirm daily `cache-budget` schedule active
- [ ] Confirm `prune-pr-cache` on PR close
- [ ] Confirm 8 GiB budget matches org limit

---

### PR3 — YAML size + optional flat names

#### WP6 — Composite extraction

- [ ] Extract shared steps into `.github/actions/velnor-ci-*` (generator-owned sources under `.github-gen/sources`)
- [ ] Restore/save/`cache-hit` gates stay in **one job** when splitting steps
- [ ] Re-measure shard boundaries; shard only when needed

#### WP-C5 — Narrow rust-policy mbx freshness (optional)

- [ ] Restrict policy freshness paths to policy-relevant files, not all `./**/*.rs`

#### Optional

- [ ] Inline aggregate jobs + composites for zero-`/`-suffix check names

---

## 4. What not to do

### Workflow structure

- Do **not** revert to lane-first kind callers (`GitHub / Rust`, `Velnor / Rust`)
- Do **not** revert to per-unit workflow **files** (50-file limit at monorepo scale)
- Do **not** keep cross-crate `needs:` inside kind reusables when using per-unit callers (silently broken ordering)
- Do **not** dual-publish old and new check names (no compatibility shim period)
- Do **not** block PR1 on composite extraction (PR3)
- Do **not** use matrix over units at aggregate level (skipped matrix legs clutter UI)
- Do **not** drop #867 both-lane contract, fork admission, or `lane_compare --strict`
- Do **not** duplicate `Control / Prepare Cargo` per crate or per lane
- Do **not** require inner comparison jobs to equal bare `unit.id` (delete Era C rule)

### Cache

- Do **not** save trusted compiler snapshots from **PR** or **fork PR** (security + budget)
- Do **not** add `pull_request` to the trusted save gate
- Do **not** skip recompile when source files changed (freshness miss is correct)
- Do **not** put `unit_job_display_name()` or caller id in cache keys
- Do **not** split restore/save/`cache-hit` gates across jobs in PR3 composites
- Do **not** fail CI on cache miss (miss is valid after lockfile/toolchain bump)
- Do **not** assume merge SHA equals PR head for exact mbx keys (documented limitation; G1 partially mitigates)

### Process

- Do **not** hand-edit `.github/workflows/*` — all changes through `velnor-workflow` + regen
- Do **not** guess runner protocol behavior — match `actions/runner` source of truth

---

## 5. Architecture reference

### Aggregation model (Option 3)

```
CI / PR (aggregate)
├─ Control / Planning
├─ Policy
├─ Control / Velnor admission
├─ Rust · velnor-runner (GitHub)  ──► ci-unit-rust.yml  (unit + lane inputs)
├─ Rust · velnor-runner (Velnor)   ──► ci-unit-rust.yml
├─ … one caller per (unit, lane) when both, every kind
├─ Control / Prepare Cargo        (Rust-only, Velnor runner)
├─ Control / Aggregate
└─ Control / Required
```

Each `workflow_call` runs one job. Dependency-closure and prep ordering live on **aggregate `needs:`**.

### Naming helper

```rust
fn unit_job_display_name(unit: &Unit, lane: Option<RunnerMode>, runners: RunnerMode) -> String {
    let base = sidebar_group_name(unit); // e.g. "Rust · velnor-runner"
    match runners {
        RunnerMode::Both => format!("{} ({})", base, lane.expect("both requires lane").display_name()),
        RunnerMode::Github | RunnerMode::Velnor => base,
    }
}
```

### GitHub platform limits (must preserve)

| Constraint | Limit | Mitigation |
|---|---|---|
| Unique reusable workflow files per caller | 50 | One file per kind, not per unit |
| Workflow file size | 500 KiB | Shard at ~480 KiB |
| Jobs per workflow | 256 | ~39 callers today; safe to ~40 units |

### Cache layers (GitHub lane, Rust)

| Layer | Transport | Restore on PR | Save on PR | Save on main |
|---|---|---|---|---|
| Rust toolchain | `actions/cache` → `~/.rustup` | Always | No | Yes |
| Compile objects | `mr-boxington-action` v1.8.3 | Always | No | Yes |
| mold linker | `actions/cache` | Always | No | Yes |
| Cargo registry/git | `actions/cache` → `~/.cargo/*` | Always | No | Yes |
| Mise tools | `mise-action cache: true` | Always | May save† | Yes |

† Align with read-only PR policy in WP-C2.

**Key formats (must not change on restoration):**

```
mbx:    velnor-mbx-v3-{12-hex-compat}-${{ runner.os }}-${{ runner.arch }}-{unit.id}-{dep-hashFiles}-{freshness-hashFiles}
cargo:  ci-${{ runner.os }}-rust-${{ hashFiles(unit.cache.key_files) }}
rustup: velnor-rustup-${{ runner.os }}-${{ runner.arch }}-${{ hashFiles('rust-toolchain.toml', 'rust-toolchain') }}
mold:   velnor-mold-2.42.0-${{ runner.os }}-${{ runner.arch }}
docker: velnor-docker-seed-v3-{digest}-…-docker-{compat-hashFiles}-{context-hashFiles}
```

### Velnor lane

Host-persistent mounts for cargo, mbx, mise. `Control / Prepare Cargo` warms shared store before parallel Rust jobs. `CARGO_NET_OFFLINE` on checks.

### Storage budget

8 GiB total; daily `maintenance.yml` `cache-budget`; PR close prunes merge-ref namespace. Parallel PRs read shared entries; they do not multiply saves.

### Scale (this repo, `runners = both`)

| Metric | Era C (now) | After restoration |
|---|---:|---:|
| Verification units | 17 | 17 |
| Top-level aggregate jobs | 15–16 | ~39 |
| Unique reusable files | 5 | 5–6 |
| `ci-required` needs entries | 13–14 | ~37 |
| `ci-unit-rust.yml` size | ~321 KiB | Similar until PR3 dedup |

---

## 6. Verification checklist

Use this to confirm work is done correctly. Check off after PR1 unless marked PR2/PR3.

### A. Workflow structure (PR1)

- [ ] **Sidebar:** PR Checks list one row per unit × lane across all kinds (`Rust · velnor-runner (GitHub)`, `Docker · Docker (Velnor)`, …)
- [ ] **Single lane:** With `runners = github` only, names have no `(GitHub)` suffix
- [ ] **Lane compare:** `velnor-tools lane-compare --strict` passes on both-lane `workflow_dispatch`
- [ ] **Limits:** ≤50 unique reusable files with 30+ synthetic Rust crates (`unique_reusable_calls_stay_under_github_limit`)
- [ ] **Shard:** 32+ pad crates produce `ci-unit-rust-2.yml` with per-unit callers (not `group-rust-2-{github,velnor,control}`)
- [ ] **Scale:** Adding a crate adds +2 aggregate callers when `both` (update `synthetic_surface.rs`)
- [ ] **Dependency closure:** `velnor_rust_needs=dependency-closure` ordering preserved at aggregate level
- [ ] **Prepare Cargo:** Velnor Rust callers `needs` prep; GitHub-lane Rust callers do not
- [ ] **Fork PR:** `Control / Velnor admission` fails; GitHub-lane unit checks still run
- [ ] **merge_group:** Both lanes run without admission job
- [ ] **Release:** `release.yml` unit verify names use `unit_job_display_name` pattern
- [ ] **Generator:** `velnor-workflow --check` clean after regen
- [ ] **Policy:** `ci-policy.yml` green
- [ ] **Lint:** `actionlint` green on generated workflows

### B. Cache reuse (PR1)

- [ ] **Golden keys:** Cache keys byte-identical to pre-restoration for fixed fixture repo
- [ ] **Restore always:** No GitHub Rust job runs checks before restore steps (automated test)
- [ ] **Restore blocks:** Every GitHub-lane job has `actions/cache/restore` or mbx setup (or Velnor bypass documented)
- [ ] **PR restore hit:** Same-repo PR with unchanged lockfile shows `cache-hit` true for rustup/cargo in logs
- [ ] **PR mbx:** mbx restore-keys hit (phase report or mbx log) on unchanged sources
- [ ] **PR no save:** No trusted save step `if:` matches `pull_request`
- [ ] **merge_group save:** Save steps include `merge_group` in `if:` (after WP-C1)
- [ ] **main producer:** main push after merge runs save steps when `cache-hit != true`
- [ ] **Parallel PRs:** Two parallel PRs both restore; cache entry count stable (no PR write growth)
- [ ] **PR close:** `prune-pr-cache` deletes merge-ref entries
- [ ] **Budget:** `maintenance.yml` cache-budget shows `total_bytes <= 8 GiB`
- [ ] **Retention check:** `velnor-workflow cache-plan --check` matches live policy

### C. Cache observability (PR2)

- [ ] Step summary or `VELNOR_CI_REPORT` shows hit/miss per layer per unit
- [ ] Daily cache-budget schedule confirmed active

### D. YAML size (PR3)

- [ ] `ci-unit-rust.yml` under shard budget after composite extraction
- [ ] Restore/save gates still in single job per unit invocation

### E. Manual smoke (post-PR1 on `tailrocks/velnor`)

1. Open a PR with no Rust changes → confirm fast restore hits in Actions logs
2. Open a PR changing one crate → confirm mbx prefix restore + recompile only affected crate
3. Run `workflow_dispatch` with `runner=both` → confirm `lane_compare --strict` green
4. Inspect Checks sidebar → confirm unit-first names, not `GitHub / Rust / …`

---

## 7. Files to change

| Area | Path | PR |
|---|---|---|
| Naming helper | `crates/velnor-workflow/src/lib.rs` | 1 |
| Aggregation, cache gates | `crates/velnor-workflow/src/primitives/ir.rs` | 1 |
| Release naming | `crates/velnor-workflow/src/primitives/release.rs` | 1 |
| Policy freshness (optional) | `crates/velnor-workflow/src/primitives/snapshot.rs` | 3 |
| Lane compare | `crates/velnor-tools/src/lane_compare.rs` | 1 |
| Tests | `crates/velnor-workflow/tests/lane_pairing.rs`, `velnor_first_ci.rs`, `synthetic_surface.rs` | 1 |
| Config | `.github-gen/velnor-workflow.toml` | 1 |
| Generated output | `.github/workflows/*` | 1 (regen) |
| Operator docs | `content/docs/guides/execution.mdx` | 2 |
| Composites | `.github-gen/sources/`, `.github/actions/velnor-ci-*` | 3 |

**Pass-through (no changes):** `velnor-runner` github_adapter, `velnor-control` query, `velnorctl get jobs`, branch rulesets (gate on `ci-required`, not leaf names).

---

## 8. Risks and mitigations

| Risk | Mitigation |
|---|---|
| Restoration breaks cache keys | WP-C0 golden tests |
| Velnor cold/offline failures | Aggregate `needs:` + prep ordering |
| merge_group never warms cache | WP-C1 |
| Parallel PRs fill storage | PR read-only saves + merge-ref prune + 8 GiB retention |
| `/ verify` suffix in Checks | Accept PR1; PR3 composites optional |
| `ci-required` script size (~37 jobs) | Regenerate `render_nodes_required()`; test validation |
| Trusted save expression drift | Single `trusted_cache_save_expression()` helper |

---

## 9. Delivery summary

| PR | Delivers | Blocks on |
|---|---|---|
| **PR1** | Unit-first names, per-(unit,lane) callers, dependency lift, merge_group saves, golden + restore tests, regen | — |
| **PR2** | Docs, cache hit visibility, retention verification | PR1 |
| **PR3** | Composite dedup, optional flat names, policy freshness | PR1 |

**Next step:** Implement PR1 per §3 and verify with §6 sections A + B.
