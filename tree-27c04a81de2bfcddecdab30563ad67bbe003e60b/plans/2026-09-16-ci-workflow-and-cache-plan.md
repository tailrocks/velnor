# CI workflow restoration and dual-lane cache plan

Status: **in progress** — branch `plan/ci-workflow-and-cache` @ `__FINAL_HEAD__` (Phases 0–6 code landed; §16 live gates open until merge + fleet ops).  
Date: 2026-09-16 (rev 11: input-parameterized O(1) kind reusables + template-memory ceiling + actionlint in Policy + revision-proven D19 guard (`--revision`) + local `setup-velnor-workflow` for the owner + runner manifest self-pin removed, pin `__FINAL_PIN__` @ `__FINAL_HEAD__`; rev 10: trust-gated aggregate skip @ `81104ba8`; rev 9: Velnor prepare-cargo prefetch + clippy @ `28b528d8` (superseded by rev 10); rev 8: prefetch package name @ `284f1091`; rev 7: docker collapsed skip @ `8e1ae640`; rev 6: kind file sharding @ `6c51eceb` (superseded by rev 10); rev 5 collapsed verify runtime + pin `8decfeeb` @ `9c1211d7`; rev 4 D19 install fix @ `ad76f4a`; rev 3 `c273707d`).  
Repository: `tailrocks/velnor`.  
Purpose: Single source for `/goal` — workflow structure + **separate GitHub and Velnor cache policies**.

Related: PR [#867](https://github.com/tailrocks/velnor/pull/867) (merged 2026-09-15T15:19Z).

Verification ledger: §20 records every claim in this plan as VERIFIED / CONTRADICTED / PARTIAL / UNVERIFIABLE with `file:line` or live-API evidence. Corrections from the ledger are already folded into §0–§19; a "(rev 2)" marker flags where the previous revision was wrong.

---

## 0. Goal statement

Deliver CI where:

1. **Checks are unit-first** — `Rust · velnor-runner / GitHub`, not `GitHub / Rust / rust-velnor-runner`. (rev 2: reusable-workflow check runs always render `<caller job name> / <callee job name>`, so the lane is the trailing segment; see D1.)
2. **Velnor lane never loses store warmth** — host-persistent cargo/mise/mbx with **50 GiB** budget; structural guarantee, not GHA key luck.
3. **GitHub lane maximizes restore within platform limits** — 8 GiB internal budget below 10 GB platform cap; strict producer/consumer; prefix restore as normal warm path.
4. **Outcomes are predictable** — observability distinguishes exact hit, prefix restore, host-warm, and cold per lane.
5. **Producers actually complete** (rev 2) — a GitHub-lane cache is only as warm as its last *finished* trusted run. Today `ci-main` finished 2 of its last 200 runs (146 cancelled, 52 failed; last success 2026-09-13T09:44Z) and `nightly` 0 of its last 15. Every "always misses" symptom downstream is dominated by this.

---

## Phase 0 — Producer health (blocking; rev 2)

Nothing in Phases 1–6 has an observable effect until these are green. All items are verified live (2026-09-16); none were in the previous revision.

| # | Breakage | Evidence | Fix |
| --- | --- | --- | --- |
| P0-1 | `Control / Planning` fails on every run: generated `.github/ci/project.toml` emits `workflow_file` **twice**, the second inside `[unit.cache]`; the runtime's `Cache` struct is `deny_unknown_fields` | `crates/velnor-workflow/src/lib.rs:912-922` (two `write_toml_string(…"workflow_file"…)` calls, the second after `[unit.cache]`); `crates/velnor-workflow/src/runtime.rs:153-158`; `.github/ci/project.toml:44,48`; introduced by `8f6f3492`; live: run 34995874327 `TOML parse error at line 48 … unknown field workflow_file` | Delete `lib.rs:920-922`. The existing test `generated_config_keeps_unit_workflow_file_outside_cache_table` (`lib.rs:9345`) would have caught it but never ran (P0-2). Regenerate. |
| P0-2 | `velnor-runner` does not compile on `main`: `recover_one_orphaned_job` references `docker_backend` / `docker` that are not in scope | `crates/velnor-runner/src/node/controller.rs:2177-2178` (from `9d0e6da1`, #865); `cargo check -p velnor-runner` → E0425 ×2. `velnor-workflow` has `velnor-runner` as a dev-dependency (`crates/velnor-workflow/Cargo.toml:32`) so its **entire test suite is uncompilable** | Thread `docker_backend` + docker command closure into `recover_one_orphaned_job`; then run `cargo test -p velnor-workflow` — expect P0-1's test to fail until P0-1 lands. |
| P0-3 | Required status check never reports: ruleset `protect-main` requires contexts `DCO` and **`ci-required`**, but #867 renamed the job to `name: "Control / Aggregate"`; `statusCheckRollup` on PRs 871/872 has no `ci-required`. Merges since 15:19Z pass only via admin bypass | `gh api repos/tailrocks/velnor/rules/branches/main` → `required_status_checks: [DCO, ci-required]`; `ci-pr.yml:245-246`; generator promise "stable `ci-required` check that repository rulesets gate on" at `ir.rs:1460-1462` | Decide the final aggregate check name once (D16), update the ruleset context in the same change, and add a fail-closed generator/tool check that every ruleset `required_status_checks.context` matches a top-level job `name` in `ci-pr.yml`. |
| P0-4 | `ci-main` producer is cancelled by the next push: `concurrency.cancel-in-progress: true` on group `velnor-<repo>-refs/heads/main-main` | `ci-main.yml:34-36`; `ir.rs:1152-1177`; live: 146/200 cancelled | Main and nightly aggregates: `cancel-in-progress: false` (queue) — see D15. PR aggregates keep `true`. |
| P0-5 | `Policy` job on `ci-main`/`nightly` fails: the pinned policy binary (`VELNOR_POLICY_WORKFLOW_REV = 4790f7cc`) rejects the current generated workflows (`pull_request_target is forbidden` for `ci-policy.yml`; "inline policy job must match the approved generated shape") → every lane is skipped on main | `crates/velnor-workflow/src/lib.rs:81`; live run 34995874327 `Policy` log: `workflow policy rejected 4 finding(s)` | Bump `VELNOR_POLICY_WORKFLOW_REV` and `VELNOR_WORKFLOW_SOURCE_REV` (`lib.rs:91`, currently `7fa4a073`, not an ancestor of HEAD in this clone) together with every generator change that alters output shape, and add a `--check`-time guard: the pinned revs must accept the generated output (run `velnor-workflow policy` from the pinned rev against the freshly generated tree). Root cause class: two self-referential pins with no coherence check. |
| P0-6 | Velnor Docker jobs can never be scheduled: `velnor_trusted_label = "velnor-host-docker"` exists **only on the 23 offline** `velnor-macos-recovery-slot-*` runners; the 5 online `velnor-dogfood-slot-*` lack it. Nightly runs sit queued ~24 h until the next schedule cancels them | `gh api repos/tailrocks/velnor/actions/runners`; `.github-gen/velnor-workflow.toml:29`; `ci-unit-docker.yml:262` `runs-on: [self-hosted, velnor-target-mvp, velnor-host-docker]`; nightly 09-07→09-12 all `cancelled` after ~24 h | Either add `velnor-host-docker` to an online trusted host, or make the `docker` unit's Velnor lane conditional on runner availability (plan-time `gh api …/runners` probe → skip with a visible reason). Queue time is not bounded by `timeout-minutes`. |
| P0-7 | GHA cache account at **10.54 GB / 109 entries (98 % of the 10 GiB cap, 1.95 GB over the 8 GiB internal budget)** despite daily maintenance: `velnor-mbx-v3-*` 22 entries / 5.34 GiB vs 3.5 GiB class; 49 entries on closed `refs/pull/*/merge` written **after** `prune-pr-cache` ran (prune fires ≈3 s after close, in-flight jobs save 1–2 min later); unclassified keys (§2.1) get no class budget | `gh api repos/tailrocks/velnor/actions/cache/usage`; `maintenance.yml` run logs `No merge-ref cache entries found` for PRs 827–856 | Phase 4: daily sweep of all `refs/pull/*/merge` scopes whose PR is closed; classify every emitted key family; make `compiler-snapshots` bound effective (22 entries ≠ gen ≤ 2 — verify `snapshot_key` grouping by unit). |
| P0-8 | `nightly` cannot warm mbx even when it finishes: `jdx/mr-boxington-action@v1.3.0` saves **only** on default-branch `push` or on `workflow_dispatch` (any ref), never on `schedule` or `merge_group` | action `src/lib.ts:246-258 shouldSave`; rendered `save-on-workflow-dispatch: true` (`ci-unit-rust.yml`) | Nightly = a scheduled job that **dispatches** `ci-main.yml` at `refs/heads/main` (`workflow_dispatch` is trusted for every gate including mbx), or upstream a `save-on-schedule` input. Remove the assumption "schedule → warm producers" from §7 until one lands. |

Phase 0 gate: 3 consecutive `ci-main` runs on `main` conclude `success`; `Control / Planning` and `Policy` green; ruleset context reports on a PR; `cargo test -p velnor-workflow` compiles and passes; `gh api …/actions/cache/usage` ≤ 8 GiB after one maintenance run.

---

## 1. Dual-lane cache philosophy

Velnor exists because **GitHub Actions cache is small, shared, and easy to evict**. The Velnor lane uses **host disk with bind mounts** — a different storage system entirely.

```text
┌─────────────────────────────────────────────────────────────────────────┐
│  SAME cache keys per unit (unit.id, hashFiles) — lane-agnostic keys   │
│  DIFFERENT transport + budget + retention per lane                      │
└─────────────────────────────────────────────────────────────────────────┘

  GitHub lane                          Velnor lane
  ───────────                          ───────────
  Transport: GHA cache API             Transport: host bind mounts
  Budget: 8 GiB (internal)             Budget: 50 GiB caches class + per-slot mbx
  Platform cap: 10 GB                  Platform cap: host disk
  Enforcer: maintenance.yml            Enforcer: velnorctl cache gc + 2 GiB floor
  mbx: backend github, objects         mbx: backend local, per-slot store
  PR: restore-only                     PR: writes to host store (trust-scoped)
  "Never miss": NOT achievable         "Never miss stores": achievable per slot
```

**Do not conflate the two budgets.** Raising Velnor to 50 GiB does **not** mean raising `RetentionPolicy.total_bytes` in `snapshot.rs` — that policy governs **GitHub Actions cache API only**.

(rev 2) **Velnor warmth is per slot, not per host.** mbx object and target stores are `/var/cache/mbx/slots/<slot>` (`crates/velnor-runner/src/container.rs:1440-1483`), because of an observed cross-container flock deadlock; mise `installs` are slot+trust+repo scoped (`container.rs:1422-1437`). Only cargo `registry/{cache,index}` + `git/db` and mise `cache` are shared across slots within a trust scope. Hit rate therefore depends on slot affinity, and "50 GiB" is `MBX_GC_MAX_TOTAL_SIZE` **per slot directory**.

---

## 2. Platform and budget reference

### 2.1 GitHub Actions cache (GitHub lane only)

| Fact | Value | Velnor policy |
| --- | --- | --- |
| Default repo storage | **10 GB**; overage is now *billable* if enabled, otherwise the cache flips read-only | Internal budget **8 GiB** (~1.3 GiB headroom) |
| Inactivity eviction | 7 days (`last_accessed_at`) | Cannot control; producers must refresh |
| Over-limit eviction | Platform LRU | Why we stay below 10 GB internally |
| Per-entry max | 10 GiB (toolkit `fileSizeLimit`) | Class budgets stay under this |
| Restore keys | max 10 | mbx uses 2, cargo 1 |
| PR writes | `refs/pull/N/merge` namespace only | velnor also blocks trusted saves on PR |
| `merge_group` scope | `refs/heads/gh-readonly-queue/<base>/pr-N-<sha>` (ephemeral; restores fall through to default branch) | Saves there are single-use — see D8 |
| Fork PR | Cannot write default-branch scope | Moot today: fork PRs are hard-failed by `Control / Velnor admission` (§3) |
| Low-trust triggers | `pull_request_target` etc. are read-only by default; new `cache-mode: read\|write\|write-only\|none` job key | Declare `cache-mode: read` on PR unit jobs (defense in depth) |
| Key max length | 512 chars | mbx hashFiles lists must fit (current rust-velnor-runner key is ~2.9 KB *before* hashing; the rendered key is short — the expression is long, the value is not) |
| Concurrent identical saves | first writer wins; loser logs `Unable to reserve cache` warning | rustup/mold keys are identical across all 13 Rust jobs — expected, harmless |

**Source:** `crates/velnor-workflow/src/primitives/snapshot.rs:341-383` → `RetentionPolicy::default_policy()`  
**Enforcement:** `.github/workflows/maintenance.yml:9` cron `31 3 * * *` → job `cache-budget` (`velnor-workflow cache-plan --mode=budget` / `--now … --entries …`); hard-fails when `failed > 0` or `total > budget` (`maintenance.yml:160-163,183-186`). No `<512 MiB` headroom warning exists (rev 2).

#### GitHub internal class table (8 GiB — keep)

| Class | Budget | Tier | Generation bound | Key markers (actual) |
| --- | ---: | --- | ---: | --- |
| toolchain-seeds | 2 GiB | Protected | 0 | `velnor-rustup-`, `velnor-mold-` |
| source-bundles | 1.5 GiB | Protected | 0 | `velnor-release-cargo-`, `velnor-cargo-`, `CiRustCargoSources` (anchored: `ci-<runner_os>-rust-…`) |
| docker-seed | 1 GiB | Baseline | 1 | `velnor-docker-seed-` |
| compiler-snapshots | 3.5 GiB | Rolling | **2** | `-mbx-v3-` |
| **Total** | **8 GiB** | | | |

(rev 2) **Unclassified key families** fall to `"unclassified"`, tier Rolling, budget 0, bound 0 (`snapshot.rs:504`) — swept only by the global pass: `mise-v1-*` (12 entries / 0.57 GiB live), `velnor-workflow-v1-*` (59 entries, saved by `setup-velnor-workflow` on every event incl. PRs, `.github/actions/setup-velnor-workflow/action.yml:231-236`), `ci-release-<os>-rust-*` (fails the anchored `CiRustCargoSources` matcher because the second segment is `release`, not the OS — `snapshot.rs:286-293`), `guest-seed-*`, `velnor-policy-mbx-1.11.1-*` (no `-mbx-v3-`), `ci-Linux-{bun,docs,opentofu}-*`. `velnor-cargo-` matches zero emitted keys (dead marker). Phase 4 classifies all of these.

- Producer window: 2 hours (`snapshot.rs:344`)
- Eviction: generation bound → class budget → global; protected never global-swept (`snapshot.rs:631-653`)
- PR cleanup: `prune-pr-cache` on PR close — **races in-flight saves** (P0-7)

### 2.2 Velnor host storage (Velnor lane only)

Canonical root `<VELNOR_STORAGE_ROOT>/cache/velnor/v1/<trust_scope>/<class>` — only when `VELNOR_STORAGE_ROOT` is set (`crates/velnor-runner/src/storage.rs:56-99`); otherwise XDG `cache/velnor` or legacy `<work>/_velnor_*`. Fleet hosts must set it (Phase 6).

| Store class | Host path (`store_catalog.rs`) | Container mount (`container.rs`) | Scope | Budget env / runtime cap |
| --- | --- | --- | --- | --- |
| cargo | `…/cargo/registry/{cache,index}`, `…/cargo/git/db` (`:152`) | `/github/home/.cargo/registry/{cache,index}`, `…/git/db` (`:574-585`); **`registry/src` + `git/checkouts` are per-job** (`:565-569`) | trust | `VELNOR_BUDGET_CARGO_BYTES` = 20 GiB (`velnorctl/src/runtime.rs:421-426`) |
| cargo bin | `…/cargo/bin` | `/github/home/.cargo/bin` (`:591-595`) | trust+repo | (cargo) |
| mise | `…/mise/` (`:165`) | `/opt/mise/installs` (slot+trust+repo, `:1422-1437`), `/opt/mise/cache` (trust, `:610-614`), `/opt/velnor/mise-binaries` | mixed | `VELNOR_BUDGET_MISE_BYTES` = 20 GiB (`:428-433`) |
| mbx compiler | `…/compiler/mbx/<repository_id>` (`:217`; `github_adapter.rs:129-174`) | `/var/cache/mbx`; `MBX_CACHE_DIR=/var/cache/mbx/slots/<slot>` (`container.rs:1440-1457`) | trust+repo+**slot** | `MBX_GC_MAX_SIZE=20GiB` per slot, `MBX_GC_MAX_TOTAL_SIZE=50GiB`, `MBX_GC_AUTO=true` (`container.rs:463-471`) |
| targets | `…/targets/` (`:178`) | `MBX_TARGET_ROOT=CARGO_TARGET_DIR=/var/cache/mbx/targets/slots/<slot>` (`:466,1466-1483`) | slot | `MBX_TARGET_MAX_SIZE=30GiB` per slot (runtime); `VELNOR_BUDGET_TARGETS_BYTES` = 200 GiB (gc only, `:400-405`) |
| sccache | `…/compiler/sccache` (`:230`) | `/var/cache/sccache` | trust | (only when mbx disabled → `MBX_DISABLE=1`) |
| actions-cache class | `…/caches/` (`:191`) | local tarball store | trust | `VELNOR_BUDGET_CACHES_BYTES` = **50 GiB** (`:407-412`) |
| artifacts | legacy `<work>/_velnor_artifacts` (`:200-205`, outside `v1`) | — | — | `VELNOR_BUDGET_ARTIFACTS_BYTES` = 20 GiB (`:414-419`) (rev 2: was missing) |
| hosted GHA emulator | `…/gha-cache/` (`:31,240-242`, no trust segment) | optional | per **namespace** | `VELNOR_GHA_CACHE_BUDGET_BYTES` = 10 GiB **per namespace** (`gha_cache.rs:76,119,129`) — total unbounded across namespaces |

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

**Enforcement (rev 2):** two mechanisms, not one. (a) **Automatic** disk-pressure reclaim at job admission when free < 2 GiB (`runner.rs:3398 DISK_MIN_FREE_BYTES`, `disk_space_problem` → `cache::reclaim_for_disk_pressure`, `DiskPressure` state machine `runner.rs:3552-3625`). (b) **Operator-only** `velnorctl cache --work-dir <dir> gc [--dry-run|--yes|--keep-newest-targets N|--max-age-days N|--max-size-bytes N]` (`velnorctl/src/runtime.rs:393-472`; note `--work-dir` is on the `cache` parent, not on `du`/`gc`). Nothing in `debian/`, `config/fleet/`, or systemd schedules (b). The `VELNOR_BUDGET_*` numbers are therefore aspirational until Phase 6 wires a timer.

**Velnor lane CI behavior today (correct — preserve):**

- `lane_enables_actions_cache` → **false** when every declared path is host-persistent (`primitives/cache.rs:139-152`); the persistent set is cargo registry/git, `/opt/mise/{installs,cache}`, `/var/cache/sccache` and the `~/.cargo`/`.cargo` aliases (`cache.rs:9-13,101-112`). **`/var/cache/mbx` is not in the set** — mbx warmth comes from `backend: local`, not from this bypass.
- mbx `backend: local` on Velnor lane (`ir.rs:3070-3099`; `ci-unit-rust.yml` Velnor jobs)
- `Control / Rust` (→ `Control / Prepare Cargo`) warms shared cargo before the Velnor Rust wave (`ci-pr.yml:209-244`; inner `velnor-prepare-cargo-sources`, `ci-unit-rust.yml:26-74`)
- Velnor Rust jobs: zero `actions/cache*` steps (verified by parse)
- (rev 2) The log line *"Cache paths live on Velnor host-persistent storage (always warm)"* lives in the **executor** (`velnor-runner/src/executor.rs:6985,7082,8186`) and only fires when an `actions/cache` step is executed — which Velnor Rust jobs no longer render. It does **not** appear in current Velnor logs (run 34969899504). Do not use it as evidence.

(rev 2) **Not host-persistent on Velnor today:** `bun-velnor` (`~/.bun/install/cache`), `docs` (`~/.npm`), `opentofu` (`~/.terraform.d/plugin-cache`) render `actions/cache/restore` on the Velnor lane (`ci-unit-bun.yml:317-325`, `ci-unit-docs.yml:315-323`, `ci-unit-opentofu.yml:322-330`), restore-only, from entries only the GitHub lane ever saves. These hit the GHA API from self-hosted runners (or the emulator) every job. Phase 1 adds them to the host-persistent mount set (WP-V1).

---

## 3. What "never miss cache" means per lane

### Velnor lane — store layers (~100% achievable **per slot**)

| Layer | Target | Mechanism |
| --- | --- | --- |
| cargo registry cache/index, git db | **Always warm** after first populate (trust-scoped, all slots) | Host bind mounts survive jobs; `registry/src` is re-extracted per job (cheap, local) |
| mise tools | **Warm per slot** | `installs` per slot; `cache` shared; `mise --yes install …` runs every job (~20 s observed) |
| mbx objects | **Hit for unchanged source on the same slot** | `backend: local`; per-slot store. Live evidence (run 34969899504, `rust-policy`): `mbx[cache]: 0 hits, 0 misses, 1 not looked up`, `mbx[savings]: 1735 compilations on file` — i.e. warm target tree, not object hits. (rev 2: the previous "WP6 evidence: 1651 hits, 0 B transfer" is **unverifiable** — no log, report, or doc in the repo contains it.) |
| mbx on source edit | Recompile, not store miss | Incremental compile — correct |
| First run / new host / new slot | One-time cold | Acceptable; N slots ⇒ N cold starts |
| Host GC | Eviction possible | mbx auto-GC per slot (20/30 GiB); 2 GiB floor; operator gc |

**Velnor does not use GHA cache for host-persistent paths.** "Never miss" = mounts + prep + offline probe, not `cache-hit=true`.

### GitHub lane — best-effort within platform (NOT 100%)

| Scenario | Expected | Fix |
| --- | --- | --- |
| Producer never finishes | **Everything cold or stale** | **Phase 0** |
| PR, unchanged lockfile | rustup/cargo exact hit (observed on PR 871: rustup, mise, mold, cargo exact; mbx prefix) | Keep producers healthy |
| PR, `.rs` change | mbx exact miss, **prefix restore** | By design; fix observability |
| PR | Never saves trusted scope | By design (D7) |
| Fork PR | **Hard-failed** by `Control / Velnor admission` (`ci-pr.yml:103-112`: `runners=both cannot omit the Velnor lane for an untrusted fork pull request`, exit 1) → `Control / Aggregate` fails | Decide (D17): keep fork PRs red by design, or run GitHub-only. Cache warmth for forks is irrelevant until decided. |
| Platform LRU / 7-day idle | Miss after eviction | main + dispatch producers |
| Compat digest rotation | Cold until main re-saves; the digest includes the unit's full command text (`ir.rs:279-280`), so any flag tweak rotates that unit's class | Ensure main saves; keep recipes stable |
| merge_group | No queue exists (`mergeQueue: null`); trigger is dead | Phase 4: remove or enable (D8) |
| mise PR writes | Budget pressure (8 of 12 live `mise-v1-*` entries are on closed PR refs) | WP-C2 |

**Realistic GitHub target:** ≥95% warm on same-repo PR with stable lockfile; prefix mbx on source edits; cold only on toolchain/schema shifts or eviction.

---

## 4. Root causes — why GitHub lane "always misses"

| ID | Root cause | Lane | Fix |
| --- | --- | --- | --- |
| RC-0 | **Producers do not complete**: `cancel-in-progress: true` on main; Planning broken (P0-1); Policy pin drift (P0-5); nightly hangs on offline `velnor-host-docker` (P0-6) | GitHub | **Phase 0** |
| RC-1 | PR restore-only consumer | GitHub | Keep; ensure producers |
| RC-2 | Observability: prefix restore reports as miss; no `cache-matched-key` read anywhere | GitHub | Phase 5 |
| RC-3 | `merge_group` never saves (and no queue exists) | GitHub | WP-C1 / D8 |
| RC-4 | Trusted save gate inlined **9×** in two shapes: 3-event (`ir.rs:919-922, 2283-2286, 2866-2869, 2992-2995`, `lib.rs:3333-3335`) and push-only (`lib.rs:2291-2294`, `release.rs:609,620,674-677,1624-1627`) | GitHub | WP-C1 |
| RC-5 | mise writes on PR: `jdx/mise-action` `cache: true` with `cache_save` left at default `true` (`ir.rs:3052-3057`) | GitHub | WP-C2 |
| RC-6 | `rust-policy` freshness = `./**/*.rs` (`ir.rs:176-177`, root `.`); `deny.toml` in `watch` but in **no** key segment | Both keys | WP-C5 |
| RC-7 | Live GHA account 10.54 GB, over 8 GiB and at 98 % of platform cap | GitHub | Phase 4 |
| RC-8 | Plan conflated 8 GiB with Velnor | Docs | Fixed |
| RC-9 | No scheduled `velnorctl cache gc` on fleet | Velnor | Phase 6 |
| RC-10 | `cache-plan --check` not implemented (only `--mode=plan\|budget`, `runtime.rs:361-378`); referenced only by this plan | GitHub | Dropped — use `--mode=plan` / `--mode=budget` |
| RC-11 | (rev 2) **Cargo cache key is per-unit**: `key_files` include `crates/<unit>/Cargo.toml`, so 13 Rust units mint 13 distinct exact keys for the **same** `~/.cargo/registry`+`git` payload; every trusted run uploads ~13 near-duplicate bundles into a 1.5 GiB class (live: 9 entries / 1.46 GiB). Key also lacks `runner.arch` (rustup/mold/mbx/docker have it) | GitHub | Phase 3: one workspace-scoped key per lockfile (`velnor-workflow-contract` has its own lockfile → second key) |
| RC-12 | (rev 2) mbx action never saves on `schedule`/`merge_group`; `save-on-workflow-dispatch` ignores ref (feature-branch dispatch saves into branch scope) | GitHub | P0-8 / D15 |
| RC-13 | (rev 2) Docker hosted PR build never consumes the restored seed (PR command `docker build --target ci …` has no `--build-context velnor-cache-seed=…`, `.github/ci/project.toml:56`), and there is **no** buildx layer cache (`--cache-from/--cache-to` absent everywhere) — every hosted PR Docker build is fully cold | GitHub | Phase 3 |
| RC-14 | (rev 2) `prune-pr-cache` races in-flight saves (P0-7) | GitHub | Phase 4 sweep |
| RC-15 | (rev 2) Unclassified key families (§2.1) escape class budgets | GitHub | Phase 4 |
| RC-16 | (rev 2) `setup-velnor-workflow` saves `velnor-workflow-v1-<os>-<arch>-<rev>` on every event including PRs (59 live entries) | GitHub | WP-C2: gate on trusted expression; key on trusted rev only |
| RC-17 | (rev 2) Cargo/docker save steps run only after a successful checks step (implicit `success()`); a red unit on main never refreshes its bundle | GitHub | WP-C3: `if: always() && (trusted) && cache-hit != 'true'` for dependency bundles |
| RC-18 | (rev 2) `~/.cargo/bin` tools (`cargo-deny`, `cargo-audit` via `taiki-e/install-action … fallback: none`, `ir.rs:3146-3158`) downloaded every run | GitHub | Phase 3: add to toolchain-seeds |
| RC-19 | (rev 2) `velnor-runner` doesn't compile → `rust-velnor-runner` and `rust-velnor-workflow` units red on main → their mbx/cargo saves never run | Both | P0-2 |
| RC-20 | (rev 2) Same-repo PR jobs write into the **trusted** Velnor stores (`cargo`, `mise/installs`, `compiler/mbx`) that main jobs consume — poisoning surface is same-repo-PR → main, not fork | Velnor | D18 |

---

## 5. Locked decisions

| # | Decision |
| --- | --- |
| D1 | Unit-first check names. (rev 2) Rendered form for a reusable job is `<caller job name> / <callee job name>`; target: caller `Rust · velnor-runner`, callee job `name: ${{ inputs.lane == 'github' && 'GitHub' \|\| 'Velnor' }}` → **`Rust · velnor-runner / GitHub`**. The previously written `Rust · velnor-runner (GitHub)` would render as `Rust · velnor-runner (GitHub) / verify`. |
| D2 | Same naming rule for all unit kinds |
| D3 | Keep **`Control / Prepare Cargo`** — Velnor-only shared warmup (today `Control / Rust` → inner `prepare-cargo`) |
| D4 | Keep #867 both-lane contract |
| D5 | One aggregate caller per (unit, lane); kind reusable files kept, **collapsed to one job per lane** keyed on `inputs.unit` + `inputs.lane` (otherwise 34 callers × 27 inner jobs = 918 skipped instances per run — the N×N problem `ir.rs:1626-1631` warns about). (rev 9) The per-unit step block is rendered **once per lane job**; every unit-specific literal is a `workflow_call` input the caller passes through `with:` (D5a below). `tests/velnor_first_ci.rs` forbids `inputs.unit == '<id>'` guards in the callee. |
| D5a | (rev 9) **Callee size is O(1) in the unit count.** GitHub loads a reusable workflow once *per calling job* into one 10 MiB `TemplateMemory` budget (actions/runner `TemplateMemory`: 24 B per token, 26 + 2×UTF-16-len per string). A callee whose body grows with the units it serves multiplies by its caller count: 27 rust callers × 233 KB (13 guarded copies of the step block) expanded to 15.91 MiB and failed every `ci-pr` run at startup with `Maximum object size exceeded`. Invariant: the generator refuses any aggregate whose expanded cost (own + Σ callee × callers) exceeds **5 MiB** (`template_memory.rs`); size-based sharding (`KIND_WORKFLOW_SHARD_BUDGET`) is removed as the wrong invariant. |
| D6 | Dependency-closure on aggregate `needs:`; dependents' `if:` must accept `skipped` upstream results exactly as `group-rust-velnor` does today (`ci-pr.yml:235`) |
| D7 | **GitHub lane:** PR read-only for trusted GHA saves |
| D8 | **GitHub lane:** `merge_group` — (rev 2) no merge queue is enabled and `merge_group` saves land in an ephemeral queue scope. Decision: **remove the dead trigger** from `ci-pr.yml` and its admission gates until a queue is enabled; when enabled, add it to the *job admission* gate only, not the save gate. |
| D9 | Cache keys keyed on `unit.id` — same keys both lanes; the **structural restoration PR (Phase 1) must not change keys**. Key improvements (RC-11, RC-6) ship in Phase 3 with their own golden update. |
| D10 | **GitHub lane budget: 8 GiB** internal — do not raise without measured overrun |
| D11 | **Velnor lane budget: 50 GiB** host (`VELNOR_BUDGET_CACHES_BYTES`, `MBX_GC_MAX_TOTAL_SIZE` per slot) |
| D12 | **Separate retention systems** — never merge GHA `RetentionPolicy` with Velnor host GC |
| D13 | Velnor lane: no GHA save steps for host-persistent paths; (rev 2) and **no GHA restore** either once bun/npm/tofu paths are host-persistent (WP-V1) |
| D14 | Observability: exact / prefix / host-warm / cold — never fail CI on miss |
| D15 | (rev 2) **Producers never cancel themselves**: `ci-main.yml` and `nightly.yml` use `cancel-in-progress: false`; nightly is a scheduled **dispatcher** of `ci-main.yml@main` so mbx saves (P0-8) |
| D16 | (rev 2) **Required-check name is a contract**: the aggregate job whose `name` the ruleset references is fixed at `ci-required` (restore the pre-#867 name) or the ruleset is updated atomically; the generator emits a check that fails when the ruleset context is absent from `ci-pr.yml` |
| D17 | (rev 2) Fork PRs: **run GitHub lane only, skip Velnor lane, do not fail admission** — the admission job becomes an informational "Velnor lane omitted for fork" notice; `ci-required` validates GitHub-lane results only for forks. (Alternative — keep red — is acceptable only if documented in `content/docs`.) |
| D18 | (rev 2) Velnor trusted stores accept writes only from **trusted events** (main push/schedule/dispatch@main); same-repo PR jobs run against a `pr` trust scope whose stores are seeded read-only from the trusted scope (overlay or copy-on-write), never the reverse |
| D19 | (rev 2) Self-referential pins (`VELNOR_WORKFLOW_SOURCE_REV`, `VELNOR_POLICY_WORKFLOW_REV`) are bumped by the generator's `--check`, which fails when the pinned rev cannot parse/accept the current output |

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

(rev 2) `RepoGenerationConfig` (`config/mod.rs:91-110`) has no cache section today and is `deny_unknown_fields`; per-unit `[units.cache]` supports `key_files`, `paths`, `mutable_mount_seed` (`:292-306`). `[cache.*]` is **generator-only** — it must never be serialized into `.github/ci/project.toml`, whose runtime parser is also `deny_unknown_fields` (P0-1 is exactly this failure mode).

Until schema lands, Velnor 50 GiB is enforced via **fleet env** (`VELNOR_BUDGET_*`, `MBX_GC_MAX_TOTAL_SIZE`).

---

## 7. GitHub lane stack (optimize within restrictions)

### Layers (as rendered today — `ci-unit-rust.yml` GitHub jobs)

| Layer | Key | Restore-keys | Save gate | Budget class |
| --- | --- | --- | --- | --- |
| rustup | `velnor-rustup-{os}-{arch}-{hashFiles(rust-toolchain.toml, rust-toolchain)}` | **none** (exact-or-cold) | 3-event trusted + miss, **before checks** | toolchain-seeds 2 GiB |
| mold | `velnor-mold-2.42.0-{os}-{arch}` (`lib.rs:93`; upstream latest 2.42.1, last C++ release before mold 3.0) | **none** | 3-event trusted + miss, before checks | toolchain-seeds |
| mise | action-managed `mise-v1-…` | action-managed | **unconditional** (`cache_save` default) | unclassified — fix WP-C2 |
| mbx | `velnor-mbx-v3-{compat}-{os}-{arch}-{unit.id}-{dep}-{fresh}` | 2 prefixes (`…-{dep}-`, `…-{unit.id}-`) | action-owned: default-branch push or any-ref dispatch; never schedule/merge_group | compiler-snapshots 3.5 GiB, gen≤2 |
| cargo | `ci-{os}-rust-{hashFiles(key_files)}` (per unit, RC-11) | `ci-{os}-rust-` | 3-event trusted + miss, after checks (success only) | source-bundles 1.5 GiB |
| docker seed | `velnor-docker-seed-v3-{digest}-{os}-{arch}-docker-{compat}-{context}` | 2 prefixes | trusted + export; collect `exit 0` silently when export absent | docker-seed 1 GiB, gen≤1 |
| runtime binary | `velnor-workflow-v1-{os}-{arch}-{rev}` (`setup-velnor-workflow`) | none | **unconditional** (RC-16) | unclassified |

Step order (verified): job start → checkout → runtime download → selection → **rustup restore/provision/save** → mise → **mbx** → **mold restore/setup/save** → cargo restore → `Prepare Cargo sources` (`if: steps.cache.outputs.cache-hit != 'true'` — exact-key only, so prefix restores still `cargo fetch --locked`) → checks → cargo save → report. (rev 2: the previous "restore → … → checks → save" oversimplified; rustup/mold save *before* checks, which is fine.)

### Trusted save gate (target — single helper `trusted_cache_save_expression(default_branch)`)

```text
(push && ref == refs/heads/main)
|| schedule
|| (workflow_dispatch && ref == refs/heads/main)
```

**Excludes:** all `pull_request` variants **and `merge_group`** (D8; rev 2). The release-side push-only family (`release.rs`, `lib.rs:2291`) is intentional (`release.rs:1615-1616`) and stays a second helper, not an inline literal.

### Producer schedule (rev 2)

```text
main push          →  primary producer (cancel-in-progress: false — D15)
nightly 03:17 UTC  →  `gh workflow run ci-main.yml --ref main` (dispatch → mbx saves)
maintenance 03:31  →  enforce 8 GiB; sweep closed-PR scopes
```

Live observation: on 2026-09-15 the scheduled runs fired at 08:40 (nightly) and 08:52 (maintenance) UTC — GitHub schedule delay — and maintenance evicted `velnor-release-mbx-v3-*` while nightly was still running. The 2-hour producer window protects only entries *created* in-window; make maintenance skip while a `ci-main`/`nightly` run is `in_progress` (`gh run list --status in_progress`).

---

## 8. Velnor lane stack (50 GiB — maximize certainty)

### Layers (as rendered today)

| Layer | Mechanism | GHA I/O |
| --- | --- | --- |
| cargo registry cache/index, git db | Host bind mounts (trust scope) | **None** |
| cargo registry src / git checkouts | Per-job home (`container.rs:565-569`) | None (local extract) |
| mise | `installs` per slot + `mise --yes install …` each job; `cache` trust-shared | **None** |
| mbx | `backend: local`, per-slot store + per-slot `CARGO_TARGET_DIR` | **None** |
| rustup/mold/mbx binaries | Image-baked (`docker/job-ubuntu.Dockerfile:83-160`; mold version from `docker/job-mise.lock`) | **None** |
| Docker | Host `dockerd` via lease socket (trusted label only); `docker build` with `sharing=locked` cache mounts (`Dockerfile:95-100`); **no named builder, no `--cache-*`, no BuildKit GC policy** | **None** |
| bun / npm / tofu plugin caches | (rev 2) `actions/cache/restore` from GitHub-lane entries, never saved from Velnor | **Restore every job** — fix WP-V1 |
| workspace | Per-job `/__w` checkout | N/A |

### Host budget allocation (50 GiB target — per host, rev 2 corrected)

| Class | Runtime cap today | Env / mechanism | Note |
| --- | ---: | --- | --- |
| mbx objects | 20 GiB **per slot** (`MBX_GC_MAX_SIZE`), 50 GiB total (`MBX_GC_MAX_TOTAL_SIZE`) | mbx auto-GC | 5 dogfood slots ⇒ up to 100 GiB objects unless total cap is honored per host |
| mbx targets | 30 GiB **per slot** (`MBX_TARGET_MAX_SIZE`) | mbx | 5 slots ⇒ 150 GiB potential; `VELNOR_BUDGET_TARGETS_BYTES` 200 GiB is gc-only |
| cargo + mise | 20 + 20 GiB | `velnorctl cache gc` only | no runtime cap; cargo stable auto-GC (1.88, `cache.auto-clean-frequency`) trims 3-month-unused downloads only and is disabled under `--offline` |
| caches class | 50 GiB | `velnorctl cache gc` only | |
| artifacts | 20 GiB | `velnorctl cache gc` only | legacy path outside `v1` |
| docker/buildkit | uncapped | `HostCapacity::promisable_bytes` subtracts a caller-supplied *growth allowance* only when docker bytes were measured (`host_capacity.rs:94-99`); admission passes `None` (`:55-57`) | add `buildkitd.toml` `[worker.oci] gc=true maxUsedSpace="40GB" reservedSpace="10GB"` on trusted docker hosts |
| headroom | 2 GiB floor | automatic reclaim | raise alert threshold to 5 GiB |

### Operational requirements

- [ ] Set `VELNOR_STORAGE_ROOT` on every fleet host so the `v1` layout is canonical (§2.2)
- [ ] `velnorctl cache --work-dir <dir> gc --yes` scheduled on fleet (systemd timer beside `velnor-fleet-policy-audit.timer`)
- [ ] `velnorctl cache --work-dir <dir> du` + `velnorctl storage status` in runbook (`storage du/gc/history/…` return "unavailable" today, `lib.rs:610-619`)
- [ ] Alert when host available < 5 GiB
- [ ] Do **not** enable `VELNOR_ACTIONS_CACHE_URL` unless non-persistent paths need it — and after WP-V1 none should
- [ ] Bring a `velnor-host-docker` runner online or remove the label requirement (P0-6)

---

## 9. Execution checklist — Phase 1: Generator core (PR1)

Pre-req: Phase 0 green.

### 9.1 Workflow structure (WP1–WP4)

- [x] Add `unit_job_display_name(unit, lane, runners)` in `lib.rs` (reuse `sidebar_group_name`'s `{kind} · {label}` form, `lib.rs:2131`)
- [x] Replace `kind_reusable_callers()` (**`primitives/ir.rs:1590`**, not `lib.rs`) with per-(unit,lane) callers; caller `if:` = `contains(needs.plan.outputs.units, unit)` so unselected units skip at the caller
- [x] Collapse each kind reusable to **one job** keyed on new `unit` + `lane` inputs; callee `name:` derived from `inputs.lane` (D1, D5); update `tests/velnor_first_ci.rs:675-676`
- [x] Regenerate `render_nodes_required()` (**`ir.rs:1701`**) for 35 caller ids (17×2 + control) plus `plan`, `velnor-lane-admission`, `policy`; keep the per-caller matrix validation semantics (`ci-pr.yml:271-413`) but generate it from one table
- [x] Rename `Control / Rust` → `Control / Prepare Cargo`; inner job `prepare-cargo` unchanged
- [x] Lift `velnor_rust_dependency_needs()` (`lib.rs:3144`) from inner jobs to aggregate `needs:`; dependents accept `skipped` (D6)
- [x] Keep `Control / Prepare Cargo` as a caller-level `needs:` of every Velnor Rust caller (today only `group-rust-velnor` needs it, `ci-pr.yml:236`)
- [x] Fix the ruleset contract (D16) and the fork-PR admission behavior (D17)
- [x] Remove the dead `merge_group` trigger and admission clauses (D8)
- [x] Rewrite `lane_compare` name parser (**`crates/velnor-tools/src/lane_compare.rs:687-724`**): parse the trailing `/ GitHub|Velnor` segment; today's fallback classifies any name containing both "velnor" and "github" as `Ambiguous`, and every Rust unit id contains "velnor"
- [x] Regen workflows; `velnor-workflow --check` green

### 9.2 GitHub lane cache fixes (WP-C0–C3)

- [x] Add `trusted_cache_save_expression(default_branch)` in `ir.rs`; replace the **5** three-event literals (`ir.rs:919, 2283, 2866, 2992`, `lib.rs:3333`); keep the release push-only family behind a second named helper
- [x] Gate `jdx/mise-action` with `cache_save: ${{ <trusted expr> }}` (WP-C2); on Velnor lane set `cache: false` (the bind-mounted `MISE_DATA_DIR` is the cache)
- [x] Gate `setup-velnor-workflow` "Save runtime cache" on the trusted expression (RC-16)
- [x] Dependency-bundle saves (`cargo`, docker seed) → `if: always() && (trusted) && steps.cache.outputs.cache-hit != 'true'` (RC-17); toolchain saves unchanged
- [x] Add `cache-mode: read` to PR-triggered unit jobs (platform defense in depth)
- [x] Golden test: cache keys unchanged for fixed fixture (D9)
- [x] Test: restore always before checks on GitHub lane
- [x] Test: Velnor lane renders zero `actions/cache*` for host-persistent units
- [x] Test: no save `if:` contains `pull_request` or `merge_group`
- [x] Bump `VELNOR_WORKFLOW_SOURCE_REV` / `VELNOR_POLICY_WORKFLOW_REV` with the coherence guard (D19)

### 9.3 Velnor lane cache preservation (WP-V0) and extension (WP-V1)

- [x] `lane_enables_actions_cache` returns false for Velnor + host-persistent paths (`cache.rs:139-152`, tests `:218-242`)
- [x] mbx renders `backend: local` on Velnor lane (`ir.rs:3070-3099`)
- [x] `Control / Rust` runs before the Velnor Rust wave via caller `needs:` (`ci-pr.yml:235-236`)
- [x] Velnor Rust jobs use the offline probe (`ci-unit-rust.yml:937-952`; generator `ir.rs:773-779`)
- [x] Docker Velnor lane has no seed restore/save (`ci-unit-docker.yml:262-432`) — but also no builder/GC contract (Phase 6)
- [x] Velnor Rust YAML has zero `actions/cache/save` and zero `actions/cache/restore`
- [x] **WP-V1**: add `~/.bun/install/cache`, `~/.npm`, `~/.terraform.d/plugin-cache` to the runner mount set and to `velnor_host_persistent_cache_path` (`cache.rs:101-112`) + `velnor-runner::executor::velnor_persistent_cache_path`; then Velnor bun/docs/opentofu jobs render no `actions/cache*`
- [x] Test: Velnor lane YAML has zero `actions/cache*` for **every** unit

### 9.4 Phase 1 gate

- [x] `velnor-workflow --check` + `velnor-workflow policy` green (rev 2: there is **no** actionlint step and `scripts/pin_integrity.mjs` is unwired — add both to `ci-policy.yml` or drop the claim) — **VERIFIED @ `ad76f4a`:** `cargo run -p velnor-workflow -- . --plain --check` green when HEAD ≠ pin (fixed `run_installed_policy` cargo install args); 453 `cargo test -p velnor-workflow` pass; pins at `abc81a94` (D19); `ci-policy.yml` still lacks actionlint / `pin_integrity.mjs`
- [ ] §16 Phase 1 items pass

---

## 10. Execution checklist — Phase 2: Generated output audit

- [x] GitHub jobs: rustup → mise → mbx → mold → cargo restore → fetch-if-miss → checks → cargo save (gated, `always()`) — **PARTIAL:** `rust-policy` GitHub lane omits `jdx/mise-action` (uses `cargo-bin` + `taiki-e/install-action` instead); other Rust units follow the stack
- [x] Velnor jobs: no GHA restore/save for any unit; mbx local; `Prepare Cargo` ordering at caller level
- [x] All save steps use the named helpers (no inline drift): assert by grep over generated YAML
- [x] `maintenance.yml`: schedule 03:31, `actions: write`, enforce 8 GiB, closed-PR sweep, skip-while-producer-running
- [x] `nightly.yml`: schedule 03:17, dispatches `ci-main.yml@main`, `cancel-in-progress: false`
- [x] `ci-pr.yml`: no `merge_group`; `cache-mode: read`

---

## 11. Execution checklist — Phase 3: Key churn (PR3)

- [x] Narrow `rust-policy` mbx freshness — drop `./**/*.rs`; freshness = `deny.toml`, `Cargo.lock` (policy commands do not compile)
- [x] Add `deny.toml` to `rust-policy` `key_files`
- [x] **Cargo bundle key per lockfile, not per unit** (RC-11): `ci-{os}-{arch}-rust-{hashFiles('Cargo.lock','rust-toolchain.toml','rust-toolchain','.cargo/**')}`; `velnor-workflow-contract` keeps its own lockfile key; expect live `ci-*-rust-*` entries to drop from 9–13 to 2
- [x] Toolchain-seeds: add `~/.cargo/bin/{cargo-deny,cargo-nextest}` (or move them into the mise-managed set already cached) — RC-18
- [x] Docker hosted PR build: pass `--build-context velnor-cache-seed=.velnor-docker-cache/seed` so the restored seed is used; add `--cache-from/--cache-to type=gha,scope=docker,mode=max` (note: 10 GiB shared budget — measure before enabling `mode=max`) — RC-13
- [x] Composite extraction; restore/save gates stay in one job
- [x] Update goldens; re-measure shard budget (`ci-unit-rust.yml` is 314 KB today because of 27 inlined jobs; after D5 it is one job) — **235 KiB** @ `c273707d` (under 480 KiB `KIND_WORKFLOW_SHARD_BUDGET`) — **(rev 9) superseded:** the per-file budget was the wrong invariant (D5a); `ci-unit-rust.yml` is **30 KB** and independent of unit count @ `114b7dfc`; expanded `ci-pr.yml` 2.71 MiB < 5 MiB ceiling

---

## 12. Execution checklist — Phase 4: GitHub budget ops

- [ ] Confirm org GHA cache limit (API exposes none; org total 52.97 GB / 14 repos, none > 10 GiB — consistent with default)
- [x] Keep `RetentionPolicy.total_bytes = 8589934592`
- [x] **Drop `cache-plan --check`** (not implemented; use `--mode=plan` / `--mode=budget` only)
- [x] Classify every emitted key family (§2.1 unclassified list) — either give each a class or stop emitting it (`velnor-cargo-` dead marker; `ci-release-*` anchored-matcher bug at `snapshot.rs:286-293`)
- [x] Daily sweep: delete all entries on `refs/pull/N/merge` for closed PRs (`gh cache delete --ref`) — fixes the prune race (RC-14)
- [ ] `compiler-snapshots`: verify generation grouping yields ≤ 2 per unit; live count is 22 entries for 13 units
- [x] `cache-plan --mode=budget` exposes per-class totals so a headroom warning (`< 512 MiB`) can be scripted
- [x] `prune-pr-cache` and `cache-budget` DELETE-failure policy made consistent (both hard-fail)
- [ ] Prove 7 consecutive daily `cache-budget` runs ≤ 8 GiB with `failed_evictions == 0`

---

## 13. Execution checklist — Phase 5: Observability (PR2)

- [x] `VELNOR_CI_REPORT.cache_outcomes.{rustup,mold,cargo,mbx,docker_seed}` = `exact|prefix|cold` using `steps.<id>.outputs.cache-primary-key` vs `cache-matched-key` (both exist in `actions/cache/restore@v6.1.0`; today nothing reads them) and mbx's own step-summary line (`exact hit / warm start / miss`)
- [x] Velnor lane reports `host_warm` per layer from the runner's persistent-path classifier (`executor.rs:7837-7848`) — emitted unconditionally, not only when an `actions/cache` step runs
- [x] `report-velnor-ci-outcomes` step summary per unit job; warn on cold, never fail CI
- [x] Update `content/docs/guides/execution.mdx` — dual-lane diagram (today it documents only mbx 20/30/50 GiB at `:222,271`)
- [x] Update `content/docs/operations/storage-and-resources.mdx` — per-slot stores, `VELNOR_STORAGE_ROOT`, gc timer
- [x] Document producer/consumer model, fork-PR decision (D17), 7-day eviction, `cache-mode`

---

## 14. Execution checklist — Phase 6: Velnor host ops (50 GiB)

### Fleet configuration

- [ ] `VELNOR_STORAGE_ROOT` set on every host
- [x] `VELNOR_BUDGET_CACHES_BYTES=53687091200` (default already)
- [ ] Confirm `MBX_GC_MAX_TOTAL_SIZE=50GiB` **per slot** is what we want, or move the total cap to a host-level gc
- [ ] Tune `VELNOR_BUDGET_CARGO_BYTES` / `VELNOR_BUDGET_MISE_BYTES` / `VELNOR_BUDGET_ARTIFACTS_BYTES` to disk capacity
- [x] systemd timer: `velnorctl cache --work-dir /var/lib/velnor/work gc --yes`
- [x] BuildKit GC policy on trusted docker hosts (§8)
- [ ] Record `velnorctl cache du` baseline after first gc

### Schema wiring (generator)

- [x] Add `[cache.github]` and `[cache.velnor]` to `RepoGenerationConfig` (`config/mod.rs`) — generator-only, never serialized to `project.toml`
- [x] `RetentionPolicy::from_config(&cache.github)` for `cache-plan`
- [x] Emit Velnor host policy artifact (`velnor.env` snippet) from generator
- [x] Golden test: adding `[cache.*]` does not change cache keys
- [x] Test: `project.toml` round-trips through the runtime parser (`deny_unknown_fields`) — the P0-1 regression class

### Trust-scope hardening (D18)

- [x] Same-repo PR jobs mount a `pr` scope overlay: read-through to trusted stores, writes stay in `pr`; trusted events write the trusted scope
- [x] mbx local store for PR scope separate from trusted scope

### Monitoring

- [ ] Alert when host disk available < 5 GiB
- [ ] Track mbx hits/misses per slot in phase reports
- [ ] Prove consecutive Velnor jobs on the same slot: `mbx[cache]` hits > 0 on unchanged source
- [ ] Prove `cache du` total ≤ 50 GiB after sustained nightly load

---

## 15. What not to do

### Workflow

- Do not revert to lane-first kind callers
- Do not revert to per-unit workflow files
- Do not hand-edit `.github/workflows/*` or `.github/ci/project.toml`
- Do not rename the ruleset-gated aggregate job without updating the ruleset in the same change (D16)
- Do not change generator output shape without bumping the self-referential pins (D19)

### Cache — GitHub lane

- Do not save trusted GHA snapshots from PR or fork PR
- Do not add `pull_request` or `merge_group` to the trusted save gate
- Do not raise GHA `RetentionPolicy` above 8 GiB without measured overrun + org limit check
- Do not gate `cargo fetch` on `cache-matched-key` (prefix may be a stale lockfile); `cache-hit` (exact) is correct and already used
- Do not fail CI on cache miss
- Do not run producers with `cancel-in-progress: true`

### Cache — Velnor lane

- Do not add GHA `actions/cache` steps for host-persistent paths — restore or save
- Do not lower Velnor host budget below 50 GiB without disk constraint proof
- Do not merge Velnor host stores into GHA `RetentionPolicy` / `maintenance.yml`
- Do not enable hosted GHA cache emulator unless non-persistent paths require it
- Do not let PR-scoped jobs write into trusted stores (D18)

### Cache — both lanes

- Do not put display names in cache keys
- Do not skip recompile when source changed
- Do not split restore/save gates across composite jobs

---

## 16. Verification gates

### Phase 0 — producer health

- [x] `cargo check -p velnor-runner` and `cargo test -p velnor-workflow` compile and pass on `main`
- [ ] 3 consecutive `ci-main` runs conclude `success`; `Control / Planning` and `Policy` green
- [ ] Ruleset `required_status_checks` context present in a PR's `statusCheckRollup`
- [ ] Nightly completes in < 2 h (no 24 h queue)
- [ ] `actions/cache/usage` ≤ 8 GiB after one maintenance run

### Phase 1 — PR1 merge

#### Workflow gates

- [x] Unit-first Checks sidebar (`<Kind> · <unit> / GitHub|Velnor`); `lane_compare --strict` green with the new parser
- [x] 6 reusable files (5 kind + `ci-release-package-signer.yml`), each kind file one job per lane; (rev 9) no shard files — template-memory ceiling test passes instead
- [x] Fork PR behaves per D17; no `merge_group` trigger

#### GitHub lane cache

- [x] Golden keys unchanged
- [x] Restore before checks (automated)
- [x] No save `if:` contains `pull_request`/`merge_group`; mise and runtime saves gated
- [ ] Same-repo PR + stable lockfile: rustup/cargo exact hit (baseline already observed on PR 871)
- [ ] Parallel PRs: GHA entry count stable (no `mise-v1-*`/`velnor-workflow-v1-*` growth on PR refs)

#### Velnor lane cache

- [x] Zero `actions/cache*` in every Velnor job (all kinds)
- [x] `Control / Prepare Cargo` ordering preserved at caller level
- [ ] Consecutive Velnor jobs: `Cargo sources warm; skipping fetch`
- [ ] mbx local hits > 0 on unchanged source on the same slot (log evidence, with slot id)

### Phase 4 — GitHub budget

- [ ] 7 daily maintenance runs: `total_bytes <= 8589934592`, `failed_evictions == 0`
- [ ] Zero entries on closed `refs/pull/*/merge` after the daily sweep

### Phase 6 — Velnor host

- [ ] `velnorctl cache du` ≤ 50 GiB after load
- [ ] Scheduled gc runs green
- [ ] Host disk headroom ≥ 5 GiB under normal load

### Manual smoke

- [ ] PR GitHub: rustup/cargo hit on doc-only change
- [ ] PR GitHub: mbx prefix restore on single-crate `.rs` change; report says `prefix`
- [ ] PR Velnor: report says `host_warm`, no fetch
- [ ] main push: save steps run and the run is not cancelled by the next push

---

## 17. Files to change

| Area | Path | Phase |
| --- | --- | --- |
| project.toml serializer bug | `crates/velnor-workflow/src/lib.rs:912-922` | **0** |
| runner compile break | `crates/velnor-runner/src/node/controller.rs:2106-2178` | **0** |
| self-referential pins + guard | `crates/velnor-workflow/src/lib.rs:81,91` | **0**, 1 |
| aggregate concurrency | `crates/velnor-workflow/src/primitives/ir.rs:1152-1177` | **0** |
| ruleset contract check | `crates/velnor-tools/src/` (new) or generator `--check` | **0** |
| callers / node ids / N×N | `crates/velnor-workflow/src/primitives/ir.rs:1554-1799` | 1 |
| display names | `crates/velnor-workflow/src/lib.rs:2110-2135` | 1 |
| dependency needs | `crates/velnor-workflow/src/lib.rs:3144` | 1 |
| lane_compare parser | `crates/velnor-tools/src/lane_compare.rs:687-724` | 1 |
| cache gates, lane render, mise/runtime save gating | `crates/velnor-workflow/src/primitives/ir.rs` | 1 |
| host-persistent set (+ bun/npm/tofu) | `crates/velnor-workflow/src/primitives/cache.rs`, `crates/velnor-runner/src/executor.rs`, `crates/velnor-runner/src/container.rs` | 1 |
| setup action save gate | `.github-gen/sources/actions/setup-velnor-workflow/action.yml` | 1 |
| tests forbidding `inputs.unit` | `crates/velnor-workflow/tests/velnor_first_ci.rs:675-676` | 1 |
| GitHub retention classes / matcher | `crates/velnor-workflow/src/primitives/snapshot.rs:286-293,341-383` | 4 |
| maintenance template (sweep, skip-while-running) | `crates/velnor-workflow/src/primitives/release.rs:2362` | 4 |
| cargo key / policy freshness / docker build-context | `crates/velnor-workflow/src/primitives/ir.rs`, `.github-gen/velnor-workflow.toml` | 3 |
| observability | `.github-gen/sources/actions/report-velnor-ci-outcomes/action.yml`, `ir.rs:361-374` | 5 |
| Dual-lane config schema | `crates/velnor-workflow/src/config/mod.rs` | 6 |
| Host GC defaults / timer | `crates/velnorctl/src/runtime.rs:393-472`, `crates/velnor-tools/debian/` | 6 |
| Operator docs | `content/docs/operations/storage-and-resources.mdx`, `content/docs/guides/execution.mdx` | 5, 6 |
| Generated workflows | `.github/workflows/*`, `.github/ci/project.toml` | 0, 1 (regen) |

---

## 18. Delivery phases for `/goal`

| Phase | Delivers | Gate |
| --- | --- | --- |
| **0** | Producers complete: serializer fix, runner compile, ruleset contract, no self-cancel, pin coherence, docker runner availability, budget under 8 GiB | §16 Phase 0 |
| **1** | Workflow structure + GitHub cache gate fixes + Velnor bypass extended to all kinds + regen | §16 Phase 1 |
| **2** | Generated YAML audit (GitHub vs Velnor stacks) | §10 |
| **3** | Key churn reduction (cargo per-lockfile key, policy freshness, docker seed use, tool bins) | §11 |
| **4** | GitHub 8 GiB budget proof + classification + closed-PR sweep | §16 Phase 4 |
| **5** | Dual-lane observability + operator docs | §13, §16 |
| **6** | Velnor fleet ops (storage root, gc timer, BuildKit GC, trust-scope overlay) + config schema | §14, §16 Phase 6 |

**Start Phase 0.** Phase 1 GitHub and Velnor cache work proceed in parallel — same keys, different transport verification.

---

## 19. Architecture reference

### Cache key formats (lane-agnostic — must not change in Phase 1)

```text
mbx:    velnor-mbx-v3-{12-hex-compat, per unit}-${{ runner.os }}-${{ runner.arch }}-{unit.id}-{dep-hashFiles}-{freshness-hashFiles}
cargo:  ci-${{ runner.os }}-rust-${{ hashFiles(unit.cache.key_files) }}          # per unit today (RC-11), no arch
rustup: velnor-rustup-${{ runner.os }}-${{ runner.arch }}-${{ hashFiles('rust-toolchain.toml', 'rust-toolchain') }}
mold:   velnor-mold-2.42.0-${{ runner.os }}-${{ runner.arch }}
docker: velnor-docker-seed-v3-{digest}-${{ runner.os }}-${{ runner.arch }}-docker-{compat-hashFiles}-{context-hashFiles}
mise:   mise-v1-{platform}-…                                                     # action-owned
rt:     velnor-workflow-v1-${{ runner.os }}-${{ runner.arch }}-{rev}              # setup action
```

### Scale (runners = both)

| Metric | Now (verified) | After Phase 1 |
| --- | ---: | ---: |
| Verification units | 17 | 17 |
| Top-level aggregate jobs (`ci-pr` / `ci-main` / `nightly`) | 15 / 16 / 16 | ~39 / ~40 / ~40 |
| Reusable-workflow callers per aggregate | 11 | 35 |
| Jobs inside `ci-unit-rust.yml` | 27 | 1 (+ `prepare-cargo`) |
| Unique reusable files | 6 (5 kind + signer) | 6 |
| GitHub GHA budget | 8 GiB (live 10.54 GB) | 8 GiB |
| Velnor host budget | 50 GiB caches class; mbx 20+30 GiB per slot | same, gc scheduled |
| Online Velnor runners | 5 dogfood (no docker label) | ≥ 1 with `velnor-host-docker` |

### Risks

| Risk | Mitigation |
| --- | --- |
| Producer never completes (dominant today) | Phase 0; D15 |
| Ruleset context drift on rename | D16 fail-closed check |
| Self-referential pin drift breaks Planning/Policy | D19 guard |
| Conflating GHA 8 GiB with Velnor 50 GiB | §1, §2 |
| GitHub restoration breaks keys | WP-C0 golden tests |
| N×N skipped jobs after per-(unit,lane) callers | D5 one-job reusables |
| Velnor cold per slot | slot affinity awareness in reports; prep ordering |
| GitHub budget overrun | daily maintenance + classification + sweep |
| Velnor disk full | per-slot mbx caps + gc timer + 5 GiB alert |
| Same-repo PR poisons trusted host stores | D18 |
| False "always miss" on GitHub | exact/prefix/cold observability |
| False "miss" on Velnor | `host_warm` from the runner classifier |

---

## 20. Verification ledger (rev 2, 2026-09-16)

Method: six parallel read-only sub-agents (budgets/retention; GitHub-lane gates and keys; workflow structure; Velnor lane; live GitHub state via `gh`; external best-practice research), each claim cross-checked against HEAD `1eff089b` and, where relevant, `origin/main` `5ca61659` and live API output. Verdicts: **V** verified, **C** contradicted, **P** partially correct, **U** unverifiable.

### Budgets, retention, host storage

| Claim (prev. revision) | Verdict | Evidence |
| --- | --- | --- |
| `RetentionPolicy::default_policy()` total 8 GiB, GHA-only | V | `snapshot.rs:341-343`; sole consumer `cache-plan` (`runtime.rs:361-378`) |
| Class table (budgets, tiers, bounds) | P | Numbers correct (`snapshot.rs:346-380`); markers incomplete (`velnor-release-cargo-`, anchored `CiRustCargoSources`); unclassified families omitted |
| Producer window 2 h; eviction order | V | `snapshot.rs:344,631-653` |
| maintenance 03:31, `actions: write`, enforces 8 GiB | V | `maintenance.yml:9,17-19,60-67,183-186` |
| Headroom warning `< 512 MiB` exists | C | none; only `headroom_bytes` in summary |
| `prune-pr-cache` hard-fails on DELETE failure | C | warns only (`maintenance.yml:57-59`); `cache-budget` hard-fails |
| nightly 03:17 | V | `nightly.yml:7` |
| `velnorctl` budgets 50/20/20/200/10 GiB at `runtime.rs:407-412` | V (+omission) | exact; `VELNOR_BUDGET_ARTIFACTS_BYTES` 20 GiB missing; GHA emulator budget is per namespace |
| `container.rs:470` `MBX_GC_MAX_TOTAL_SIZE=50GiB` | V | plus `MBX_GC_MAX_SIZE=20GiB`, `MBX_TARGET_MAX_SIZE=30GiB`, per-slot dirs (`:458-471,1440-1483`) |
| Host paths/mounts table | P | root canonical only with `VELNOR_STORAGE_ROOT`; cargo mounts are sub-directories; `src`/`checkouts` per job; mise `installs` per slot |
| `velnorctl cache gc --yes`, `cache du --work-dir`, `storage status`, 2 GiB floor | P | `--work-dir` is on `cache`, not `du`; `storage du/gc/…` unavailable; floor `host_capacity.rs:25` |
| `HostCapacity` subtracts docker | P | subtracts a caller-supplied growth allowance; admission passes `None` |
| `cache-plan --check` missing but referenced in docs | P | missing (V); docs reference (C) — only this plan mentions it |
| No `[cache.*]` in `RepoGenerationConfig` | V | `config/mod.rs:91-110` |
| Docs mention budgets / dual-lane / 7-day | C | `storage-and-resources.mdx:99-117` mbx sizes only; no 8 GiB, no dual-lane, no eviction |
| No scheduled `cache gc` | V | none in `debian/`, `config/fleet/`, `scripts/` |

### GitHub lane gates and keys

| Claim | Verdict | Evidence |
| --- | --- | --- |
| `trusted_cache` inlined 6+ places | P | 9 sites in two shapes (§4 RC-4) |
| Save gate lacks `merge_group` | V | no save `if:` contains it; job admission gates do |
| `trusted_cache_save_expression` absent | V | 0 hits |
| mise-action writes on PR | V | `ir.rs:3052-3057`; action defaults; live 8/12 entries on closed PR refs |
| Key formats §19 | V (nuances) | compat digest is per unit; rustup/mold have no restore-keys |
| `rust-policy` freshness `./**/*.rs` | V | `ir.rs:176-177`; `deny.toml` in no segment |
| Restore before checks; order | P | rustup/mold save before checks (fine) |
| Saves gated trusted + miss | V | `ci-unit-rust.yml` saves |
| "Do not use `cache-matched-key` to skip fetch" (implies risk exists) | C | generator already uses exact `cache-hit` (`ir.rs:726-729`) |
| mbx action gating | P | v1.3.0 saves on default-branch push or any-ref dispatch only |
| Docker: layer cache export without `ignore-error` | C | no buildx layer cache anywhere; seed is a `type=local` tarball; PR build ignores seed |
| `actions/cache` split restore/save pinned | V | v6.1.0 `55cc8345…` |
| Cache outcome observability exists | P | `report-velnor-ci-outcomes` has phases + mbx grep, no exact/prefix/cold |

### Workflow structure

| Claim | Verdict | Evidence |
| --- | --- | --- |
| Current names `GitHub / Rust / rust-velnor-runner` | V | live run 34990359947 |
| `kind_reusable_callers()` in `lib.rs` | P | exists in `ir.rs:1590`; callers per kind+lane with `selected_units` CSV, no matrix |
| `unit_job_display_name` absent | V | |
| Reusables lack `unit` input; jobs `{lane}-{unit}` | V | 27 jobs in `ci-unit-rust.yml` |
| `render_nodes_required()` in `lib.rs`, ~37 ids | P | `ir.rs:1701`; 11 caller ids today; 35 after (+3 prerequisites) |
| `Control / Rust` = Velnor-only shared warmup | V | `ci-pr.yml:209-244`; inner `prepare-cargo` |
| `velnor_rust_dependency_needs()` inner today | V | `lib.rs:3144`; `ir.rs:2175-2181` |
| `lane_compare --strict` | P | lives in `velnor-tools`; parser hard-codes lane-first |
| 17 units / 15–16 jobs / 5 reusables | P | 17 / 15–16 / **6** |
| Fork admission correct | P | fork PRs are **failed**, not just excluded |
| `merge_group` present | V (dead) | `ci-pr.yml` only; no queue enabled |
| Concurrency group | P | per-workflow suffix appended; PR number doubled; producers `cancel-in-progress: true` |
| Regen gate runs first | P | via `.github/ci/project.toml:249-257`, not literally in YAML |
| policy + actionlint gate | P | no actionlint; `pin_integrity.mjs` unwired |

### Velnor lane

| Claim | Verdict | Evidence |
| --- | --- | --- |
| `lane_enables_actions_cache` false when host-persistent | V (+scope) | `/var/cache/mbx` not in set; docker/mixed paths not bypassed |
| mbx `backend: local` | V | |
| "always warm" log line in generator output | C | executor-only, not emitted by current Velnor Rust jobs |
| Prepare Cargo ordering | P | caller-level only; inner jobs do not `needs:` it in both-mode |
| Offline probe | V | |
| Retained BuildKit builder, no seed | P | no builder/GC contract; host dockerd only |
| rustup/mold image-baked | V | |
| Zero `actions/cache/save` for host-persistent units | P | true for Rust; bun/docs/opentofu restore from GHA on Velnor |
| Mount table | C | sub-directory, per-slot, trust-scoped (§2.2) |
| "WP6 evidence: 1651 hits, 0 B" | U | no source in repo |
| `host_warm` reporting exists | C | nothing emits it |
| `velnor-host-docker` runners available | C | label only on offline runners |

### Live state (2026-09-16)

| Claim | Verdict | Evidence |
| --- | --- | --- |
| PR #867 both-lane contract | V (merged) | 48 files; post-merge no run shows both lanes executing |
| GHA account exceeded 8 GiB (RC-7) | V | 10,541,848,684 B / 109 entries |
| main push = primary producer | C | 2/200 successes; `cancel-in-progress: true` |
| nightly = warm producer | C | 0/15 successes; hangs on offline docker runners; mbx never saves on schedule |
| Ruleset gates `ci-required` | V (broken) | context absent from PR check runs since #867 |
| Org limit default 10 GB | U (consistent) | API exposes no limit |

### External facts (best-practice research)

| Fact | Status |
| --- | --- |
| 10 GB default, billable overage, 7-day eviction, LRU, 512-char key, 10 restore keys, per-entry 10 GiB | confirmed |
| `actions/cache` latest v6.1.0; `save-always` deprecated (never worked) | confirmed — repo pin is current |
| `cache-hit` exact-only; `cache-primary-key` / `cache-matched-key` outputs | confirmed |
| `cache-mode: read\|write\|write-only\|none` job key; low-trust triggers read-only by default | confirmed (new) |
| `merge_group` runs on `gh-readonly-queue/*` refs; restores fall through to default branch | confirmed |
| `jdx/mise-action` v4.3.0: `cache_save` default true; `cache_restore_keys` does not exist | confirmed |
| mold latest 2.42.1 (last C++ release); `rui314/setup-mold` exists | confirmed — pin 2.42.0 is one patch behind |
| Cargo stable auto-GC (1.88, `cache.auto-clean-frequency`), disabled under `--offline`; no size cap | confirmed |
| BuildKit GC: `[worker.oci] gc, reservedSpace, maxUsedSpace, minFreeSpace` (`gckeepstorage` outdated) | confirmed |
| `Swatinem/rust-cache` skips `registry/src`, prunes `target/` deps only; `save-if`, `cache-on-failure` | confirmed — matches WP-C3 rationale |

### Branch `plan/ci-workflow-and-cache` @ `c273707d` (2026-09-16)

Method: local code/tests/generated YAML at `c273707d`; live gates remain **U** until merge and green runs. Verdicts: **V** verified locally, **P** partial, **U** unverifiable (live-only).

#### Phase 0 — producer health (code)

| Claim | Verdict | Evidence |
| --- | --- | --- |
| P0-1 duplicate `workflow_file` in `[unit.cache]` removed | V | `lib.rs:927-928` single emit; `generated_config_keeps_unit_workflow_file_outside_cache_table` in suite |
| P0-2 `velnor-runner` compiles | V | `cargo check -p velnor-runner` exit 0 @ `c273707d` |
| P0-3 ruleset contract `ci-required` | V | `ci-pr.yml:600-601`, `ci-main.yml:623-624`; `validate_ruleset_required_status_checks` (`lib.rs:3251`) |
| P0-4 main/nightly `cancel-in-progress: false` | V | `ci-main.yml:36`, `nightly.yml:41` |
| P0-5 pin coherence D19 | V | `VELNOR_*_REV = e6fabca` (`lib.rs:83,93`); `validate_pinned_revision_coherence` (`lib.rs:4534`) |
| P0-6 docker skip when no online `velnor-host-docker` | V | `.github-gen/velnor-workflow.toml:32` `velnor_trusted_runner_available = false`; `ci-unit-docker.yml:271-274` `verify-velnor-trusted` `&& false`; release skip job comment |
| P0-7 live account ≤ 8 GiB | U | live 10.54 GiB; sweep/classify code landed, not yet proven post-maintenance |
| P0-8 nightly dispatches `ci-main@main` for mbx saves | V | `nightly.yml:48` `Control / Dispatch ci-main` |
| Phase 0 gate: 3× green `ci-main` on `main` | U | blocked until merge; `origin/main` still red pre-merge |
| Phase 0 gate: ruleset context on PR rollup | U | DCO + merge pending; context unproven on open PR |
| Phase 0 gate: `actions/cache/usage` ≤ 8 GiB | U | live 10.54 GiB |

#### Phase 1 — generator core

| Claim | Verdict | Evidence |
| --- | --- | --- |
| Per-(unit,lane) callers; one job per kind reusable (D5) | V | `ir.rs:1850` `unit_lane_callers`; `ci-unit-rust.yml` `verify-github` / `verify-velnor` only |
| Unit-first names + `lane_compare` trailing parser (D1) | V | `lib.rs:2201`; `lane_compare.rs:687-724`; 30 `cargo test -p velnor-tools lane_compare` pass |
| `Control / Prepare Cargo`; dependency `needs:` lifted (D3,D6) | V | `ci-pr.yml:224`; `ir.rs:1941-1956`; `velnor_first_ci.rs:727` |
| Fork PR D17; no `merge_group` D8 | V | `ci-pr.yml:108-110`; no `merge_group:` in `ci-pr.yml` |
| Trusted cache helpers WP-C0–C3; WP-V1 host-persistent paths | V | `ir.rs:737`; `cache.rs:104-120`; tests `github_lane_save_gates_*`, `velnor_lane_yaml_omits_*` |
| `velnor-workflow --check` | V | `force_generation_adopts_the_declared_surface_and_check_is_complete` pass |
| `cargo test -p velnor-workflow` | V | 453 passed @ `c273707d` |
| actionlint + `pin_integrity.mjs` in `ci-policy.yml` | P | generated `.github/actionlint.yaml`; wired in `mise.toml` check, not CI policy job |
| §16 Phase 1 live gates (sidebar, PR cache hits, mbx hits) | U | require green merged runs |

#### Phase 2 — generated output audit

| Claim | Verdict | Evidence |
| --- | --- | --- |
| GitHub stack ordering | P | `ci-unit-rust.yml` rust-policy block: rustup→mbx→cargo-bin→mold→cargo; **`rust-policy` omits `mise-action`** |
| Velnor zero GHA cache all kinds | V | `velnor_lane_yaml_omits_actions_cache_for_every_supported_unit_kind` |
| maintenance skip-while-producer + sweep + 8 GiB enforce | V | `maintenance.yml:75-141,163-186` |
| nightly dispatcher + queue concurrency | V | `nightly.yml:7,41,48` |

#### Phase 3 — key churn

| Claim | Verdict | Evidence |
| --- | --- | --- |
| `rust-policy` freshness = `deny.toml`/`Cargo.lock` (not `./**/*.rs`) | V | `ir.rs:212-220`; mbx key in `ci-unit-rust.yml:198` |
| Cargo key per lockfile + arch (RC-11) | V | `ci-unit-rust.yml:289` `ci-${{ runner.os }}-${{ runner.arch }}-rust-${{ hashFiles(...) }}` |
| Docker seed consumed on hosted PR (RC-13) | V | `.github/ci/project.toml:55` `--build-context velnor-cache-seed=…` + GHA buildx cache |
| Toolchain `velnor-cargo-bin-*` seeds (RC-18) | V | `ci-unit-rust.yml:206-215`; `lib.rs:3476` |
| Shard budget after collapse | C (rev 9) | Neither the 480 KB nor the 120 KB per-file budget modelled GitHub's limit, which is per-run template memory with the callee counted once per caller: 27 callers × 233 KB expanded to 15.91 MiB > 10 MiB; three 120 KB shards still expanded to ~8 MiB. Replaced by D5a: `ci-unit-rust.yml` 30 KB, `ci-pr.yml` expanded 2.71 MiB (`template_memory.rs`); no shard files |

#### Phase 4 — GitHub budget ops

| Claim | Verdict | Evidence |
| --- | --- | --- |
| Classify emitted key families (§2.1) | V | `snapshot.rs:346-426`; `emitted_key_families_classify_into_retention_classes` |
| Closed-PR merge-ref daily sweep (RC-14) | V | `maintenance.yml:97-141`; prune hard-fail `:61-63` |
| `budget.json` headroom + per-class totals | V | `snapshot.rs:498-524`; `maintenance.yml:163-170` |
| DELETE-failure policy consistent | V | both `prune-pr-cache` and sweep hard-fail on delete miss |
| `compiler-snapshots` ≤ 2 per unit live | U | algorithm tests (`github_fractional_created_at_ages_mbx_generations`); live 22 entries / 13 units unverified |
| 7 consecutive daily runs ≤ 8 GiB | U | live gate |

#### Phase 5 — observability

| Claim | Verdict | Evidence |
| --- | --- | --- |
| `cache_outcomes` exact/prefix/cold in report action | V | `.github/actions/report-velnor-ci-outcomes/action.yml:223-325`; `report_action_classifies_cache_outcomes_*` |
| Velnor `host_warm` per layer | V | `ir.rs:453-481`; action `host_warm_layer()` |
| Dual-lane docs + producer/fork/cache-mode | V | `content/docs/guides/execution.mdx:218-239`; `storage-and-resources.mdx:38,132` |
| Live `cache_outcomes` in run log | U | unproven until green CI run post-merge |

#### Phase 6 — Velnor host ops

| Claim | Verdict | Evidence |
| --- | --- | --- |
| `[cache.*]` schema; `velnor-host.env` emission | V | `config/mod.rs:110-143`; test `emitted_project_toml_ignores_cache_generation_config` |
| Fleet timer + runbook + BuildKit GC artifact | V | `debian/velnor-cache-gc.{timer,service}`; `config/fleet/RUNBOOK.md`; `buildkitd.gc.toml` |
| D18 PR read-through overlay | V | `trust_class.rs:172-253`; `storage.rs:186-199`; `velnor-host.env:10` |
| `VELNOR_STORAGE_ROOT` applied on fleet hosts | U | snippet only; no host apply evidence |
| Online `velnor-host-docker` runner | U | `velnor_trusted_runner_available = false`; 0 online @ rev 2 live audit |
| mbx hits / `cache du` ≤ 50 GiB / disk alert | U | live gates |

#### Open blockers @ `c273707d`

| Blocker | Status |
| --- | --- |
| `ci-main` green on `main` (3×) | U — merge pending |
| GHA account ≤ 8 GiB after maintenance | U — 10.54 GiB live |
| 7 daily `cache-budget` successes | U |
| mbx hits on same slot | U |
| Fleet host apply (`VELNOR_STORAGE_ROOT`, gc timer, BuildKit) | U |
| DCO on PR | U |

#### Rev 4 delta @ `ad76f4a` (2026-09-16)

| Claim | Verdict | Evidence |
| --- | --- | --- |
| D19 `run_installed_policy` uses valid `cargo install` syntax | V | `lib.rs:4470-4482` `velnor-workflow --bin velnor-workflow`; was invalid `--package` |
| `velnor-workflow --check` when HEAD ≠ pin | V | `cargo run -p velnor-workflow -- . --plain --check` exit 0 @ `ad76f4a` (HEAD `f54d2f76` ≠ pin `abc81a94`) |
| Policy on PR pre-merge | P | `pull_request_target` runs `ci-policy.yml` from `main` (pin `4790f7cc`); expected red until merge |
| PR #872 DCO | V | pass after `git rebase origin/main --signoff` + force push @ `c3917fb5` |

#### Rev 5 delta @ `9c1211d7` (2026-09-16)

| Claim | Verdict | Evidence |
| --- | --- | --- |
| PR CI instant-fail (0 jobs) root cause | P (rev 9) | The orphan `if:`-only step was a real syntax error but **not the cause of the persisting startup failure**: after the rev-5 fix (and the step-id prefixing in `69f94748` / workflow-level `cache-mode` in `6b60fa4e`, neither of which was causal — `cache-mode` is valid workflow- and job-level syntax) every `ci-pr` run still failed at `ci-pr.yml#L429` with `Error from called workflow …/ci-unit-rust.yml: Maximum object size exceeded`. True root cause: per-caller template memory (D5a, Rev 9 delta) |
| Collapsed verify runtime bootstrap | V | `render_unit_runtime` in `render_collapsed_lane_verify_job`; actionlint clean on all `ci-unit-*.yml` |
| Pins @ `8decfeeb` (D19) | V | `lib.rs:83,93`; generated runtime artifact names updated |
| `cargo test -p velnor-workflow` | V | 453 passed @ `9c1211d7` |

#### Rev 6 delta @ `6c51eceb` (2026-09-16)

| Claim | Verdict | Evidence |
| --- | --- | --- |
| PR CI `startup_failure` (0 jobs) — parsed object cap | V | GitHub run 35026888820: `Maximum object size exceeded` on `ci-unit-rust.yml@630b264e`; single collapsed file ~256 KB / ~230 steps |
| Kind reusable file sharding | V | `KIND_WORKFLOW_SHARD_BUDGET = 120_000` (`ir.rs:37`); emits `ci-unit-rust.yml`, `ci-unit-rust-2.yml`, `ci-unit-rust-3.yml` |
| Per-shard `prepare-cargo` caller ids | V | `prepare_cargo_caller_job_id_for_file` (`lib.rs:2216`); `ci-pr.yml` has `prepare-cargo`, `prepare-cargo-2`, `prepare-cargo-3` (no duplicate YAML keys) |
| Duplicate collapsed step ids (HTTP 422) | V | unit-prefixed step ids via `qualified_step_id`; reverted per-job verify sharding (`630b264e`) |
| Invalid workflow-level / caller `cache-mode: read` | V | removed; PR triggers default to read-only cache |
| Pins @ `6c51eceb` (D19) | V | `lib.rs:83,93`; `cargo test -p velnor-workflow` 453 passed |
| PR CI jobs start (not `startup_failure`) | V | run 35027858205 / 35028632413: 41–66 jobs scheduled; no `Maximum object size exceeded` |
| rust-velnor-workflow GitHub offline install | V | run 35027857785: fixed by online policy prefetch in Prepare Cargo (`ir.rs`); pins @ `0cf84f08` |
| velnor-runner/tools fmt CI failures | V | `cargo fmt` on `container.rs`, `trust_class.rs`, `fleet_policy.rs` @ `0cf84f08` |
| DCO after rebase | V | `git rebase origin/main --signoff`; pins rebumped @ `a986f028` |

#### Rev 7 delta @ `8e1ae640` (2026-09-16)

| Claim | Verdict | Evidence |
| --- | --- | --- |
| P0-6 collapsed docker Velnor skip (concurrency unblock) | V | `render_collapsed_lane_verify_job` applies `append_trusted_runner_availability_gate` (`ir.rs:2589-2604`); `ci-unit-docker.yml:271-274` `&& false` + skip comment |
| `collapsed_trust_gated_docker_lane_skips_when_trusted_runner_is_unavailable` | V | `lib.rs:7098`; `cargo test -p velnor-workflow` 454 passed |
| Pins @ `8e1ae640` (D19) | V | `lib.rs:83,93`; release golden digests updated |

#### Rev 8 delta @ `284f1091` (2026-09-16)

| Claim | Verdict | Evidence |
| --- | --- | --- |
| Stuck PR CI concurrency (run 35027857785) | V | `gh run cancel --force`; run 35030781895 reached 93 jobs / 66 scheduled |
| Policy prefetch `cargo install` package name | P | `lib.rs:4509` `velnor-workflow --bin velnor-workflow`; fixes run 35030781895 `Rust · velnor-workflow / GitHub` multi-binary error — **superseded by rev 9**: the prefetch step is removed; the guard consumes the pinned binary the job already has |
| Fork aggregate gate D17 cleanup | V | `ir.rs:2208-2219`; github callers require success; velnor callers accept skipped on fork PR |
| Pins @ `284f1091` (D19) | V | `lib.rs:83,93`; `cargo test -p velnor-workflow` 455 passed |
| Run 35031834569 Planning green + docker skip visible | V | `Control / Planning` pass; `Docker · Docker / Velnor · skipped (no online velnor-host-docker runner)` |
| rustfmt + manifest pin drift (run 35031834569) | V | `6130a77e` rustfmt; `manifest.rs` @ `f3fc75a`/`28b528d8`; the self-pin is **deleted in rev 10** |

#### Rev 9 delta @ `28b528d8` (2026-09-16)

| Claim | Verdict | Evidence |
| --- | --- | --- |
| Velnor prepare-cargo policy prefetch for `--plain --check` | V | `ir.rs:2700-2708` `append_lane_cargo_prep_jobs`; `ci-unit-rust-2.yml:77-81` |
| Clippy clean (`velnor-workflow --all-targets -D warnings`) | V | local `cargo clippy -p velnor-workflow --all-targets -- -D warnings` exit 0 |
| Prefetch bash drops redundant `cd` (no duplicate workspace hop) | V | `lib.rs:4508-4511`; regen `ci-unit-rust-2.yml` single `cd -- "$GITHUB_WORKSPACE"` before install |
| Pins @ `f3fc75a` / D19 @ `28b528d8` | V | `lib.rs:83,93`; `manifest.rs:712`; release golden digests; `cargo test -p velnor-workflow` 455 passed |
| §16 `ci-required` / policy green / 3× main green | P | run 35036454380: docker skip OK; ci-required failed `velnor-rust-policy` (fleet admission); fmt fix @ `b6f7f5e2` |

#### Rev 10 delta @ `81104ba8` (2026-09-16)

| Claim | Verdict | Evidence |
| --- | --- | --- |
| ci-required accepts trust-gated Velnor docker skip | V | `ir.rs:3658-3660,2212-2230`; run 35035084210 root cause `velnor-docker skipped`; `trust_gated_velnor_docker_skip_is_accepted_by_ci_required` |
| lane_pairing clippy format_push_string | V | `92305dd2` `write!` fix in `lane_pairing.rs:468` |
| Pins @ `643f3312` / D19 @ `81104ba8` | V | `lib.rs:83,93`; `cargo test -p velnor-workflow` 456 passed |
| Run 35036454380 trust-gated docker in aggregate | V | `velnor-docker: skipped` accepted; no `velnor-docker did not pass: skipped` |
| Run 35036454380 velnor-workflow GitHub | P→V | fmt drift `lib.rs:7140`; fixed @ `b6f7f5e2` |
| Run 35036454380 Velnor admission failures | P | 4/31 Velnor jobs: `operational store rejected the sanitized admission row` (`runner.rs:7266`); fleet ops |

#### Rev 11 delta @ `__FINAL_HEAD__` (2026-09-16)
Root cause of the persisting `ci-pr` startup failure (`Invalid workflow file: .github/workflows/ci-pr.yml#L429 … Error from called workflow …/ci-unit-rust.yml@…: Maximum object size exceeded`): GitHub loads a called reusable workflow **once per calling job** into one 10 MiB `TemplateMemory` budget. The collapsed rust callee carried 13 copies of the per-unit step block (`if: inputs.unit == '<id>'`, unit-prefixed step ids; 233 KB / 4291 lines) and had 27 callers. Neither `cache-mode` (valid syntax, GA 2026-09-10; actionlint 1.7.12 lags it) nor the step ids were causal. Rev-5's "instant-fail root cause" row is downgraded to PARTIAL above; rev 6's file sharding (`KIND_WORKFLOW_SHARD_BUDGET = 120_000`, `ci-unit-rust-{2,3}.yml`, per-shard `prepare-cargo-N`) treated the symptom and is removed.
| Pre-fix expansion over GitHub's budget | V | estimator over the `6b60fa4e` tree: `ci-pr.yml` own 155.4 KiB + 27 × 583.8 KiB (`ci-unit-rust.yml`) + 2 × ~50 KiB (bun/docker/docs/opentofu) = **15.91 MiB** > 10 MiB |
| Callee is O(1) in unit count (D5a) | V | `ci-unit-rust.yml` 31 341 B (was 233 KB / 4291 lines); bun 20.2 KB, docker 23.1 KB, docs 20.1 KB, opentofu 20.3 KB @ `b4c46874`; no `inputs.unit == '` in any callee (`tests/velnor_first_ci.rs`, `velnor-workflow-contract/tests/cache_keys.rs`) |
| Post-fix expanded totals | V | @ `b4c46874`: `ci-pr.yml` **2.77 MiB** (own 200.2 KiB; 27 × 81.8 KiB rust; 2 × 52.4/58.5/52.1/52.8 KiB); `ci-main.yml` **2.79 MiB**; `nightly.yml` **2.79 MiB**; `release.yml` 596.6 KiB; `preview.yml` 151.7 KiB — all under the 5 MiB ceiling (GitHub 10 MiB) |
| Fail-closed template-memory validator | V | `template_memory.rs`: mirrors `TemplateMemory` (24 B/token, 26 + 2×UTF-16 per string); runs in generation and `--check`; unit tests incl. 30 × 400 KiB synthetic aggregate refused with contributors listed |
| Cache key values unchanged (D9) | V | `velnor-workflow-contract/tests/cache_keys.rs` resolves each caller's `with:` into the callee's `hashFiles(inputs.*)` / `${{ inputs.* }}` keys and asserts equality with the pre-change literals (`tests/fixtures/pre_parameterization_cache_keys.rs`, 17 hosted (unit, lane) rows) |
| Sharding removed | V | `KIND_WORKFLOW_SHARD_BUDGET`, `flush_kind_workflow_shard`, `Unit.workflow_file`, stem-based matrix outputs gone; matrix outputs are `<kind>_matrix` |
| `ci-required` verdict script | V | `[[ "$FORK_PR" == true && false ]]` (SC2158; `false` is a non-empty string, so hosted jobs accepted `skipped` on fork PRs) → fork-skip branch rendered only for Velnor lane jobs |
| actionlint in Policy | V | `inline_policy_job_for_lane`: `jdx/mise-action` `install_args: actionlint@1.7.12` (hosted) / `mise --yes install` (Velnor) + `mise exec actionlint@1.7.12 -- actionlint` in `policy-checkout`; sparse checkout adds `.github/actions`; `.github/actionlint.yaml` ignores only `unexpected key "cache-mode" for "(workflow|job)" section`; scan refuses a `mise.lock` actionlint pin ≠ `ACTIONLINT_VERSION` |
| actionlint clean | V | `actionlint .github/workflows/*.yml` exit 0 @ `b4c46874` (was 83 findings: 72 SC2158/SC2160, 10 bool-to-string input type, 1 `cache-mode`); with `cache-mode` gone from callers (rev 7/8) `.github/actionlint.yaml` carries only the runner-label allowlist |
| Live failure A root cause (job 104590109433, `Rust · velnor-workflow / GitHub`) | V | `run_installed_policy` could only obtain the pinned binary by `cargo install --git`, but the regen-gate unit runs `mbx run … --plain --check` under `CARGO_NET_OFFLINE=true`; rev 8's online prefetch patched the symptom (and first failed with `multiple packages with binaries found`). Prefetch removed |
| D19 guard proves the binary it runs | V | `build.rs` stamps git `HEAD` as `VELNOR_WORKFLOW_SOURCE_SHA` (`lib.rs:81` `SOURCE_REVISION`; `velnor-workflow --revision`, `version --json`); `resolve_pinned_policy_binary` (`lib.rs:4657`) accepts only a binary whose `--revision` equals the pin, in order: `VELNOR_WORKFLOW_PINNED_BINARY` (wrong revision → error, no fallback), every `velnor-workflow` on PATH, the guard's own install root, then `cargo install --git … velnor-workflow --bin velnor-workflow --rev <pin>`; under `CARGO_NET_OFFLINE=true` it refuses with every candidate and its reported revision listed |
| Pinned binary reaches the offline unit | V | hosted Planning provisions the pinned runtime (`setup-velnor-workflow` `rev: <pin>`) next to the event runtime and ships it in the runtime artifact as `velnor-workflow-policy` (`manifest.json` `policy_revision`/`policy_binary_sha256`); `Download Velnor workflow runtime` verifies the digest and `--revision`, installs it, exports `VELNOR_WORKFLOW_PINNED_BINARY` (`ci-unit-rust.yml`) |
| Offline verification of the exact unit command | V | HEAD `b4c46874` ≠ pin `c0d04300`; `cd crates/velnor-workflow && mbx run --locked --manifest-path Cargo.toml -- --plain --check ../..` with `CARGO_NET_OFFLINE=true`: exit 0 via env (a) and via PATH (b); wrong env binary → `reports revision b4c46874…, but the pinned … is c0d04300…` exit 1; nothing pinned → `no velnor-workflow binary built at the pinned … CARGO_NET_OFFLINE=true forbids installing one (…candidates…)` exit 1 |
| Stamp cannot lie across checkouts | V | cargo keyed the build-script fingerprint by package, so a target dir shared by two checkouts kept the first checkout's `.git/…/HEAD` paths and stamped the second with the first's commit (observed: `1df3fc16` reported from a `f91b39a9` checkout); `build.rs` now names a never-existing `OUT_DIR` sentinel and re-runs every build (`c0d04300`) |
| Live failure B root cause (`release_workflow_action_refs_are_compiled_into_the_manifest`) | V | `crates/velnor-runner/src/manifest.rs` compiled `ActionCapability { repository: "tailrocks/velnor", allowed_refs: [8e438e06…, 7fa4a073…] }` — a third self-referential pin that every D19 bump invalidated. Removed: the owner's `release.yml`/`preview.yml`/`maintenance.yml`/aggregates run `uses: ./.github/actions/setup-velnor-workflow` with `rev:` (`workflow_setup_action_uses`, `lib.rs:3416`; consumers keep the remote pin); `maintenance.yml` `cache-budget` gained the sparse checkout the local action needs; the capability entry is deleted. Velnor admits local composites without a capability: `manifest::violations_with_context_limited` skips `./` refs and `action::local_action_plans` resolves them from the workspace |
| Pins @ `c0d04300` (D19) | V | `lib.rs:100,110`; `--check` exit 0 with HEAD `b4c46874` ≠ pin (pinned binary built from the `c0d04300` checkout; the install-from-git path needs the commits pushed) |
| Tests | V | `cargo test -p velnor-workflow` 416 lib + 50 integration @ `b4c46874`; `velnor-workflow-contract` 6 (D9 fixture: `rust-velnor-workflow` freshness list gained `crates/velnor-workflow/build.rs`, the scan fact the literal tree renders too); `cargo nextest run -p velnor-runner --lib --features test-support` see push report; `cargo fmt --all --check` clean |
| `velnor-workflow` clippy gate | V | the unit's `clippy --profile test --all-targets --all-features -- -D warnings` failed with 24 findings (13 on the shared branch before this series); all fixed behaviour-preservingly (`1b7adf7d`, `--plain --force` leaves the tree clean); `velnor-runner` clippy gate exit 0 |
| Dead monolithic render path | U | `#[allow(dead_code)]` over `impl WorkflowIr` (from `c2bc9c57`) hides `render`, `render_nested_unit`, `render_lane_job`, `render_verify_*` — the pre-reusable aggregate shape no generated file uses; ~70 lib tests still assert on it. Separate migration: delete the path and port the tests to `generated_files` |
| Further headroom | U | splitting each kind reusable per lane would halve the per-caller cost (~1.4 MiB for `ci-pr.yml`); not needed under the ceiling |
