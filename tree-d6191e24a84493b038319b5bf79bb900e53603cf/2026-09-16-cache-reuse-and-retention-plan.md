# Cache reuse and retention plan

Status: **approved direction** (implement with workflow restoration).  
Scope: GitHub-hosted and Velnor CI for `tailrocks/velnor`.  
**Master plan:** [`2026-09-16-ci-master-plan.md`](2026-09-16-ci-master-plan.md) (unified workflow + cache delivery).  
Related: [`2026-09-16-workflow-structure-restoration.md`](2026-09-16-workflow-structure-restoration.md), [`fleet/ci-cd-cache-architecture-evidence.md`](../fleet/ci-cd-cache-architecture-evidence.md).

---

## 1. Goal

Maximize cache reuse on **every CI run** (PR, merge queue, main, nightly) so we:

- Do not re-download Cargo registry/git, toolchains, or Docker seeds when a compatible entry exists
- Do not recompile crates when Mr. Boxington object snapshots still match
- Do not exceed the GitHub Actions cache account budget when many PRs run in parallel
- Preserve correct behavior after the workflow structure restoration (per-unit callers)

**Non-goals (impossible or wrong):**

- Saving trusted compiler snapshots from **fork PRs** (security: untrusted writers)
- Skipping recompile when **source files changed** (freshness segment must miss — that is correct)
- Unlimited cache growth (bounded by retention policy + PR merge-ref prune)

---

## 2. Current architecture (verified 2026-09-16)

### 2.1 GitHub lane — five cache layers per Rust job

| # | Layer | Transport | Restore on PR? | Save on PR? | Save on main push? |
|---|---|---|---|---|---|
| 1 | Rust toolchain | `actions/cache` → `~/.rustup` | Always | No | Yes |
| 2 | Compile objects | `mr-boxington-action` objects v1.8.3 | Always | No* | Yes* |
| 3 | mold linker | `actions/cache` → `~/.cache/velnor/mold` | Always | No | Yes |
| 4 | Cargo registry/git | `actions/cache` → `~/.cargo/*` | Always | No | Yes |
| 5 | Mise tools | `mise-action cache: true` | Always | May save† | Yes |

\* mbx saves follow action + trusted-event rules; PR runs restore only.  
† Inconsistent with other layers — see WP-C2.

**Key formats (must not change on restoration):**

```
mbx:    velnor-mbx-v3-{12-hex-compat}-${{ runner.os }}-${{ runner.arch }}-{unit.id}-{dep-hashFiles}-{freshness-hashFiles}
cargo:  ci-${{ runner.os }}-rust-${{ hashFiles(unit.cache.key_files) }}
rustup: velnor-rustup-${{ runner.os }}-${{ runner.arch }}-${{ hashFiles('rust-toolchain.toml', 'rust-toolchain') }}
mold:   velnor-mold-2.42.0-${{ runner.os }}-${{ runner.arch }}
docker: velnor-docker-seed-v3-{digest}-…-docker-{compat-hashFiles}-{context-hashFiles}
```

Keys use **`unit.id`**, not job display names. Restoration renames (`Rust · velnor-runner (GitHub)`) do **not** invalidate cache.

### 2.2 Trusted save gate (today)

All `actions/cache/save` steps and Docker seed collect/save use:

```yaml
(github.event_name == 'push' && github.ref == 'refs/heads/main')
|| github.event_name == 'schedule'
|| (github.event_name == 'workflow_dispatch' && github.ref == 'refs/heads/main')
```

**Not included:** `pull_request`, `merge_group`.

| Event | Restore | Save | Role |
|---|---|---|---|
| PR (same-repo) | Yes | No | Read shared cache warmed by main |
| PR (fork) | Yes | No | Read only; never write trusted entries |
| merge_group | Yes | **No** ← gap | Full build but cache not persisted |
| push → main | Yes | Yes | **Primary cache producer** |
| schedule (nightly) | Yes | Yes | Secondary producer + retention trigger |
| workflow_dispatch @ main | Yes | Yes | Manual full-scope runs |

### 2.3 Storage budget and eviction

| Mechanism | File | Behavior |
|---|---|---|
| Account budget | `snapshot.rs` `RetentionPolicy::default_policy()` | 8 GiB total |
| Per-class budgets | toolchain 2 GiB, cargo sources 1.5 GiB, docker seed 1 GiB, mbx rolling 3.5 GiB | Protected classes evicted last |
| Generation bound | mbx: 2 generations per variant; docker seed: 1 | Old generations eligible for eviction |
| Scheduled maintenance | `maintenance.yml` `cache-budget` | Daily: plan + apply evictions via `velnor-workflow cache-plan` |
| PR merge-ref prune | `maintenance.yml` `prune-pr-cache` | On PR close: delete `refs/pull/N/merge` cache namespace |
| Producer window | 2 hours | Newest generation protected from eviction |

Multiple parallel PRs **read** the same main-warmed entries; they do **not** each allocate new save slots (PR does not save). Storage growth comes from **main/nightly saves** and **freshness key churn**, bounded by retention.

### 2.4 Velnor lane

| Store | Mechanism |
|---|---|
| Cargo registry/git | Host bind mounts — always warm, no GHA I/O |
| Mr. Boxington | `backend: local` — slot-persistent |
| Mise | Host `/opt/mise/*` mounts |
| Prep | `Control / Prepare Cargo` warms shared store before parallel Rust jobs |

Velnor: cache miss = slower fetch, not failure (`cargo fetch` before `CARGO_NET_OFFLINE` checks).

### 2.5 Docker (GitHub)

| Event | Behavior |
|---|---|
| PR | Restore seed → `docker build --target ci` (validate only, no export/save) |
| main/nightly | Restore seed → full build → export tarballs → save seed |

---

## 3. Gaps to close

| ID | Gap | Impact | Priority |
|---|---|---|---|
| **G1** | `merge_group` not in trusted save gate | Merge queue runs full builds but never persists mbx/cargo/docker saves | **P0** |
| **G2** | `trusted_cache` expression duplicated in 4+ places in `ir.rs` | Drift risk when fixing G1 | **P0** |
| **G3** | `mise-action` may save on PR while other layers are read-only | Extra cache entries from PR runs; wastes budget | **P1** |
| **G4** | No automated assertion that restore steps always emit | Regression could drop restore silently | **P1** |
| **G5** | `merge SHA ≠ PR head` cache correlation (fleet §9.2 BLOCKED) | Merge queue may miss PR-era exact keys | **P1** (partially fixed by G1) |
| **G6** | `rust-policy` mbx freshness = all `**/*.rs` | Any Rust edit invalidates policy compile cache | **P2** (narrow freshness) |
| **G7** | Phase reports log cache-hit but no CI gate on hit rate | Cold runs invisible until slow | **P2** |
| **G8** | Restoration could break cache if unit lookup or `needs:` wrong | Velnor ordering / wrong keys | **P0** (restoration WP) |

---

## 4. Work packages

### WP-C0 — Cache-safe workflow restoration (blocks PR1)

**Owner:** workflow restoration PR1. **No new cache behavior** — preserve existing keys and steps.

| # | Task | File |
|---|---|---|
| C0.1 | Render cache steps from `inputs.unit` → correct `CacheSpec` | `ir.rs` `render_lane_job_for_input` |
| C0.2 | Never put `unit_job_display_name()` or caller id in cache keys | `ir.rs`, `snapshot.rs` |
| C0.3 | Lift `velnor_rust_dependency_needs` to aggregate `needs:` | `lib.rs`, `ir.rs` `render_node_callers` |
| C0.4 | Keep single `Control / Prepare Cargo` before Velnor Rust callers | `ir.rs`, aggregates |
| C0.5 | Golden test: regen unchanged → identical cache keys + `hashFiles(...)` args per unit | `velnor-workflow` tests |
| C0.6 | PR3 composites: restore/save/`cache-hit` gates stay in **one job** | future WP4 |

**Acceptance:** `velnor-workflow --check` clean; golden cache-key test passes; no change to key strings for fixed fixture repo.

---

### WP-C1 — Unify trusted save expression + add merge_group (P0)

**Problem:** Merge queue is a trusted, full-scope event (`runtime.rs` treats it as trusted) but saves are blocked.

**Change:**

1. Add single helper in `ir.rs`:

```rust
fn trusted_cache_save_expression(default_branch: &str) -> String {
    format!(
        "(github.event_name == 'push' && github.ref == 'refs/heads/{default_branch}') \
         || github.event_name == 'schedule' \
         || github.event_name == 'merge_group' \
         || (github.event_name == 'workflow_dispatch' && github.ref == 'refs/heads/{default_branch}')"
    )
}
```

2. Replace all inline `trusted_cache` string literals in:
   - `render_lane_job_for_input` (cargo save)
   - `render_mold_cache_steps`
   - `render_rustup_cache_steps`
   - `render_mutable_mount_seed_steps` (Docker collect + save)
   - `render_verify_lane` / release paths if they share the gate

3. **Do not** add `pull_request` to save gate — PRs must remain read-only for trusted snapshots (fork safety + budget).

4. Regenerate workflows; add test asserting generated YAML contains `merge_group` in save `if:` conditions.

**Acceptance:** merge_group run saves mbx + cargo + docker seed when `cache-hit != true`; PR save steps still exclude `pull_request`.

---

### WP-C2 — Align mise-action with read-only PR policy (P1)

**Problem:** `mise-action` with `cache: true` may write cache entries on PR runs while mbx/cargo/docker do not.

**Options (pick one in implementation):**

| Option | Change |
|---|---|
| **A (recommended)** | Pass mise cache key explicitly; gate mise save with same `trusted_cache_save_expression` via env or wrapper if action supports it |
| **B** | Set `cache: false` on PR-triggered jobs only (restore via prior main save through action's restore path if available) |
| **C** | Document as acceptable PR noise; rely on retention to evict |

**Acceptance:** After 10 same-repo PR runs without main push, Actions cache entry count does not grow from mise-only keys (measure in maintenance summary).

---

### WP-C3 — Restore-always contract tests (P1)

**Problem:** Nothing fails CI if a generator change drops `actions/cache/restore` or mbx setup.

**Change:**

1. Test per kind file: every GitHub-lane job body contains:
   - `actions/cache/restore` OR documented bypass (Velnor host-persistent)
   - `mr-boxington-action` OR unit kind without Rust compile
2. Test: no GitHub Rust job runs checks before restore steps (step order assertion)
3. Test: `Prepare Cargo sources` gated `if: steps.cache.outputs.cache-hit != 'true'` for pinned-lockfile units

**Acceptance:** Tests fail if restore block removed from generator template.

---

### WP-C4 — Cache hit visibility in CI (P2)

**Problem:** Cold runs are only visible in phase timing reports, not gated.

**Change:**

1. Extend `report-velnor-ci-outcomes` or phase report to emit structured `cache_hit: {rustup,mold,cargo,mbx}` in `VELNOR_CI_REPORT`
2. Optional: `velnor-tools` summary command for run ID showing hit/miss per unit
3. Do **not** fail CI on cache miss (miss is valid after lockfile/toolchain bump) — warn only

**Acceptance:** GitHub step summary shows hit/miss per layer for each unit job.

---

### WP-C5 — Narrow rust-policy mbx freshness (P2)

**Problem:** Policy unit freshness `hashFiles` includes all `./**/*.rs` — any crate edit invalidates policy mbx exact key.

**Change:** Restrict freshness inputs to policy-relevant paths (workspace manifests, `deny.toml`, policy scripts) in scan or unit override in `.github-gen/velnor-workflow.toml`.

**Acceptance:** Edit to unrelated crate does not change `rust-policy` mbx freshness segment.

---

### WP-C6 — Retention hardening (P1)

**Already implemented** — verify and document:

| Check | Action |
|---|---|
| Daily `cache-budget` job runs | Confirm schedule `31 3 * * *` active |
| Failed eviction fails job | Already enforced (`maintenance.yml` L160) |
| PR close prunes merge-ref | Already enforced (`prune-pr-cache`) |
| Budget = 8 GiB | Confirm `velnor-workflow cache-plan --mode=budget` matches org limit |

**Optional enhancement:** Alert when `headroom_bytes` in retention summary < 512 MiB (generator or workflow step).

---

## 5. Event matrix (target state after WP-C1)

| Event | Restore all layers | Save mbx/cargo/docker | Save mise | Storage impact |
|---|---|---|---|---|
| PR same-repo | Yes | No | No (after WP-C2) | None (read-only) |
| PR fork | Yes | No | No | None |
| merge_group | Yes | **Yes** (WP-C1) | No | One generation per variant |
| push main | Yes | Yes | Yes | Bounded by retention |
| nightly | Yes | Yes | Yes | Bounded by retention |
| workflow_dispatch @ main | Yes | Yes | Yes | Bounded by retention |

**Multiple PRs in parallel:** Each restores from the same main-warmed keys. No save → no multiplication of cache entries. Merge-ref namespaces pruned on PR close.

---

## 6. What “always reuse cache” means in practice

| Situation | Expected behavior |
|---|---|
| PR with same lockfile, no toolchain bump | Cargo registry **hit**; mbx **prefix or exact hit**; minimal fetch |
| PR after main warmed cache overnight | **Best case** — full restore from main saves |
| PR changing only docs (no Rust units selected) | Rust cache steps not run (unit not selected) — correct |
| PR changing `.rs` in crate X | mbx exact miss for crate X → **prefix restore** + recompile changed crate only |
| Lockfile bump on PR | New cargo key → fetch required once; main push saves new entry |
| mbx compat digest bump (toolchain/mbx version) | Cold mbx until main rebuilds — **expected**, not a bug |
| merge_group after WP-C1 | Saves persist → main push may exact-hit |
| Account near 8 GiB | Maintenance evicts rolling mbx generations; toolchain + cargo sources protected |

We **cannot** skip recompile when source changed — that would serve stale artifacts. We **can** ensure restore always runs and saves happen on every trusted full-scope event.

---

## 7. Implementation order

```
WP-C0  Cache-safe restoration     ──┐
WP-C1  merge_group save gate       ├── PR1 (single merge recommended)
WP-C3  Restore-always tests       ──┘
WP-C2  mise PR save alignment          PR1 or PR1.1
WP-C6  Retention verification          PR1 (docs only if already green)
WP-C4  Hit visibility                  PR2
WP-C5  rust-policy freshness           PR2 (optional)
```

**Combined PR1 deliverables:**

1. Workflow structure restoration (unit-first names, per-unit callers)
2. `trusted_cache_save_expression()` + merge_group saves
3. Cache key golden tests + restore-always tests
4. Regenerated `.github/workflows/*`

---

## 8. Verification checklist (post-merge)

Run on `tailrocks/velnor` after PR1:

- [ ] Same-repo PR: rust job logs show `cache-hit` true for rustup/cargo on unchanged lockfile
- [ ] Same-repo PR: mbx restore-keys hit (phase report or mbx log)
- [ ] main push after PR: save steps run (`Save "Rust crate …" cache` not skipped entirely)
- [ ] merge_group run (or simulate): save steps include `merge_group` in `if:`
- [ ] Fork PR: no save steps match `pull_request` in trusted gate
- [ ] Close PR: `prune-pr-cache` deletes merge-ref entries
- [ ] `maintenance.yml` cache-budget: `total_bytes <= budget`, evictions applied
- [ ] `velnor-workflow cache-plan --check` matches live retention policy
- [ ] Two parallel PRs against main: both restore, neither increases cache count materially
- [ ] Restoration: cache keys byte-identical to pre-restoration for same fixture repo

---

## 9. Files to change

| File | WP | Change |
|---|---|---|
| `crates/velnor-workflow/src/primitives/ir.rs` | C0,C1 | `trusted_cache_save_expression()`; merge_group in save gate; per-unit cache render |
| `crates/velnor-workflow/src/lib.rs` | C0 | `aggregate_velnor_rust_needs()`; cache key golden tests |
| `crates/velnor-workflow/src/primitives/snapshot.rs` | C5 | Policy freshness paths (optional) |
| `crates/velnor-workflow/tests/` | C0,C3 | Golden keys, restore-always, merge_group save assertion |
| `.github/workflows/*.yml` | all | Regenerate |
| `plans/2026-09-16-workflow-structure-restoration.md` | — | Cross-link (done) |

---

## 10. Summary

| Concern | Answer |
|---|---|
| Will PRs reuse cache? | **Yes** — restore always runs; reads main/nightly entries |
| Will PRs fill storage? | **No** — PR does not save trusted entries; merge-ref pruned on close |
| Will merge queue warm cache? | **After WP-C1** — currently gap G1 |
| Will restoration break keys? | **No** — if WP-C0 golden tests pass |
| Will we never recompile? | **No** — source changes require recompile; mbx prefix still speeds deps |
| Budget enforcement? | **Yes** — 8 GiB policy + daily maintenance + PR prune |

**Recommendation:** Ship WP-C0 + WP-C1 + WP-C3 together in PR1. That gives maximum cache reuse for parallel PRs, closes the merge_group gap, and protects against restoration regressions.
