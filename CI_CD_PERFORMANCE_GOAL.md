# CI/CD performance goal (v3)

Status: NOT ACHIEVED. Previous revisions let agents claim success without
measurements. This revision is falsifiable: no claim counts without the
before/after table in §8. Core rule for PRs: test ONLY affected units plus
their correct transitive closure — never unaffected units (§4–§6 define the
exact configuration to get there).

Objective: cut PR and main CI wall time while preserving every required
check, coverage, and compatibility. Fix the generator
(`crates/velnor-workflow`) first, then regenerate. Never hand-edit
`.github/workflows` (see `.github/workflows/AGENTS.md`). Obey repo
`AGENTS.md`.

## 1. SLOs

| Metric | Baseline 2026-09-06 | Target |
| --- | --- | --- |
| PR gate wall (plan start → `ci-required` green), affected scope | 12m05s (run 34022845304) | ≤ 6m warm |
| Slowest Rust PR job (`velnor-runner`) | 11m39s, run step 11m10s | ≤ 5m warm |
| Docker PR job | 9m18s (release build 7m46s inside) | ≤ 4m |
| Rust setup overhead per job (mise + nextest + mold) | ~25–30s after cache hit | ≤ 10s |
| PR jobs with usable build-cache restore | 0 of 10 Rust jobs (all `No mbx cache found`) | ≥ 90% prefix restores |
| Main push wall (no queue) | ~25m, plus queue pile-ups | ≤ 15m |
| Nightly wall | 1h33m (run 34020370896) | measure, then budget |

Targets are proposed, not proven. An agent may revise a target only with
new measurements attached.

## 2. How to measure (mandatory protocol)

```bash
gh run view <RUN> --repo tailrocks/velnor --json jobs,conclusion,createdAt,updatedAt
gh run view <RUN> --repo tailrocks/velnor --job <JOB> --log | grep -E \
  "No mbx cache|first build|mbx\[cache\]|Downloading crates|Finished .* profile|test result"
gh cache list --repo tailrocks/velnor --limit 100 | grep -E "velnor-mbx|mise-v1"
gh api /repos/tailrocks/velnor/actions/caches --paginate --jq \
  '[.actions_caches[]] | {count: length, bytes: map(.size_in_bytes) | add}'
```

Cache signals per Rust job log:

| Signal | Meaning |
| --- | --- |
| `No mbx cache found` + `first build on this machine` | cold: full registry download + compile |
| `Downloading crates ...` | registry cache missed |
| `Finished dev/test profile in …` (two of them: clippy + nextest) | double compilation per job |
| `mbx[cache]: H hits, M misses, 0 B downloaded, 0 B uploaded, X stored` | end-of-job cache account |

Reference baselines (PR run 34022845304, branch
`codex/release-v0.1.263-20260906`, scope `affected`, all GitHub lane):

| Job | Total | Run step | Split | Cache |
| --- | --- | --- | --- | --- |
| velnor-runner (101458506956) | 11m39s | 11m10s | clippy/dev 5m00s + test 5m31s + exec 35s (1685 tests) | cold |
| velnorctl (101458506934) | 10m43s | 10m07s | dev 4m51s + test 5m11s + exec 2s | cold |
| velnor-bench (101458507063) | 9m00s | 8m28s | dev 4m04s + test 4m19s + exec 2s | cold |
| Docker (101458506866) | 9m18s | 8m54s | release compile 7m46s of 4 binaries | cache-from only |
| velnor-tools (101458506938) | 4m39s | 4m14s | — | cold |
| workflow / control / render / client / model / unit-collector | 53s–1m56s | 25–82s | — | cold |
| bun / docs / opentofu / policy | 9–49s | — | — | ok |

Compilation is 94–99.5% of the slow Rust jobs. Warm remainder is ~1–2 min
(test exec + ~28s setup). Cache account 2026-09-06: 108 entries, 11.88 GiB
used vs ~10 GiB limit — over budget (see §3 R1).

## 3. Root causes (ranked, verified 2026-09-06 by 8 subagents + 2 verifiers)

R1 — Build cache never warm on PRs. All 10 Rust jobs cold on 2026-09-06.
Verified facts:
  a. Upstream `mr-boxington-action` semantics (README): PRs — including
     same-repo — are RESTORE-ONLY even with `save-on-workflow-dispatch:
     true`. Saves happen only on default-branch pushes and trusted
     dispatches. So PR warmth must come from main; main must save on
     every push (it does), and prefix `restore-keys` must hit (did not:
     keys rotated on every `Cargo.lock` bump + cache over budget).
  b. Budget overdrawn: 17 `velnor-mbx-*` entries = 7.25 GiB (10 stable
     5.11 GiB + 7 legacy per-commit orphans 2.14 GiB, suffix
     `-a1e07a28…`, never re-hit since the stable-key change) plus ~4.1 GiB
     Docker buildkit blobs. GitHub LRU must evict ~1.9 GiB of warm weight.
  c. Every release-bump PR changes `Cargo.lock`, which is in 12 units'
     `hashFiles` keys (docker, rust-policy, all 10 other rust units) —
     exact misses on the most common PR type. Prior attempt PR #580
     (closed; key stabilization) is live and did NOT fix wall time.
  d. `mise install --locked rust` re-downloads 6 toolchain components
     (~17s) on EVERY Rust job despite the mise cache hit.
  e. nextest + mold binaries download on every job, uncached (~1–2s × 10).
  f. `setup-velnor-workflow` miss path compiles the runtime from source
     (~3–6 min); dormant today (cache hits) but one rev-bump from firing.

R2 — `affected` selects ~everything. PR #581 ran all 15 units. Verified:
  a. `crates/velnor-workflow/**` is watched by ALL 15 units (generator
     `lib.rs:2434`; `project.toml:38,52,63,77,91,105,119,134,149,164,178,
     193,208,223,237`). Any generator change = full CI without ever
     hitting a fallback.
  b. Docker watches `crates/**` + `tools/**` (`project.toml:52`): ANY crate
     file (source, test, README, fixture) rebuilds the image.
  c. `Cargo.lock` + root `Cargo.toml` watched by 12 units; a version bump
     (own manifest + lockfile + changelog) selects 13/15 (all but bun and
     opentofu; changelog `*.md` pulls docs via `**/*.md`).
  d. Docs unit watches repo-wide `**/*.md` plus nonexistent `docs/**` and
     `mkdocs.yml`; real docs are `content/docs/**/*.mdx` (20 files) which
     match NOTHING → full fallback. Crate READMEs trigger markdownlint
     noise; real doc edits trigger full CI. Both directions wrong.
  e. Opentofu `**/*.tf` matches exactly one file: a test fixture
     (`crates/velnor-workflow/tests/fixtures/polyglot/infra.tf`).
  f. Single-crate source change (e.g. `velnor-client/src/*`) correctly
     expands via dependents+prerequisites to ~7 rust units + docker — but
     every expanded unit runs the FULL suite (no build-only tier), and
     shared crates recompile per job (R3).
  g. Degrade-to-full triggers in `runtime.rs`: empty/unset/zero BASE_SHA,
     git-diff failure, any `.github/` path, any unmatched file
     (`debian/**`, `fleet/**`, `schemas/**`, `docker/job-*` match nothing
     → full), empty diff hard-ERRORS (fails plan on no-op PRs), two-dot
     diff with possibly stale base over-selects, and unit jobs re-resolve
     on shallow checkouts (no `fetch-depth` in reusable workflows) so
     re-resolution silently degrades to full every time.

R3 — Shared dependencies compiled once per unit. Per-unit isolated caches
rebuild `velnor-model` etc. inside every dependent; each job builds twice
(clippy dev + nextest test). No cross-unit sharing by key design.

R4 — Docker PR job rebuilds release binaries (7m46s, 4 bins in one layer
after `COPY crates ./crates`). PR is `--cache-from` only, never
`--cache-to`; any `.rs` change invalidates the layer; no manifest-prefetch
split.

R5 — Slow tests dominate warm time: `idle_resource_scaling…` 20.1s,
`oauth_client_assertion…` 11.9s, `supervised_controller…` 8.3s,
`actual_sqlite_lock` 5.1s, `complete_job_retries_5xx` 5.1s.

R6 — Self-hosted lane zero throughput: 15/15 Velnor jobs `queued` forever
on main run 34023394569 and nightly 34020370896; with main
`cancel-in-progress: false` this wedges main and Nightly queues (50min–
2h+). PR lane unaffected (Velnor skipped, auto-cancel on).

R7 — Nightly duplicates main CI with no `ci-required` fan-in and no
alerting; `merge_group` maps to `affected`, so queue combinations merge
without ever being tested together; plan/job double-resolution can
silently no-op a needed unit (`return Ok(())`, accepted as pass).

R8 — Hidden file couplings the manifest graph cannot see (adversarial
review, all confirmed): `velnor-tools` `include_str!`s
`../../velnor-runner/src/manifest.rs` in a TEST while depending only on
client+model; `velnorctl` re-exports runner scaffold yet omits
`microvm/**` from its watch; `env!("CARGO_PKG_VERSION")` compiled into
model/runner/ctl-facing output (version-only ≠ no-op); `build.rs`
release identity gate (`tag == version == lock`) never runs in PR CI;
`--all-features` per-crate green proves neither default-feature nor
workspace-unified builds (`mise test-production-topology` exists but is
not in CI); `docker/job-*.Dockerfile`, `docker/job-mise.*` unwatched.

## 4. Verified affected-only design (target configuration)

Principle: a PR runs a unit IFF (unit's own files changed) OR (a unit it
depends on changed and it must re-verify) OR (a broad-impact file
changed). Everything else skips with zero runner cost (already true:
unselected reusable calls are scheduler-skipped, `steps=[]`).

D1. Scope engine (`crates/velnor-workflow/src/runtime.rs`):

- Single resolution: plan computes the unit list once; unit jobs consume
    it verbatim (pass allowlist through). Fail closed on SHA mismatch;
    log no-ops as `::warning::` with both SHAs. Eliminates the
    plan/job double-read + shallow-checkout silent-full path (keep
    `fetch-depth: 0` on plan only).
- `merge_group` → `full` (one-line change in `scope_for_event_values`).
    Required BEFORE any narrowing.
- Empty diff → empty selection (vacuous pass), not `Err`.
- Three-dot diff `base...head` instead of two-dot.
- KEEP fail-closed fallbacks exactly as coded: empty/zero base → full,
    git failure → full, any `.github/` path → full, any unmatched file →
    full. Never allowlist the unmatched fallback — it is the only guard
    for `debian/**`, `fleet/**`, `schemas/**` until D2 watches land.
- Remove the dead prebuilt `GlobSet` / per-file `Glob` recompile.

D2. Watch graph (`crates/velnor-workflow/src/lib.rs` + `project.toml`):

- Docker: replace `crates/**` + `tools/**` with Dockerfile-parsed inputs
    and packaged crates (`velnor-runner`, `velnorctl`, `velnor-tools`,
    `velnor-workflow`) + their `depends_on` closure; extend watch to
    `docker/**` (covers `job-*.Dockerfile`, currently → full fallback).
- Generator: replace the all-15 `crates/velnor-workflow/**` blanket with
    classification — selection-logic diffs fan out; template-only diffs
    select `rust-velnor-workflow` + regen-idempotence gate + one consumer
    per changed template shape. Narrowing to "workflow unit only" alone
    is UNSAFE (runtime binary executes every job).
- Docs: watch real docs (`content/docs/**/*.mdx`, root `*.md`), drop
    repo-wide `**/*.md` + nonexistent `docs/**`/`mkdocs.yml`.
- Add missing watches: `debian/**` → runner+ctl; `schemas/**` →
    model+dependents; `fleet/**` → tools+runner; `microvm/**` → ctl;
    `velnor-tools` += `crates/velnor-runner/src/manifest.rs`
    (`include_str!` edge). Teach the generator to parse `include_str!`
    and fail when a target is watch-uncovered.
- Opentofu: exclude `**/tests/fixtures/**`. Bun: narrow `**` globs to
    `src/**`, `scripts/**`, package files.
- KEEP `Cargo.lock` + root `Cargo.toml` + toolchain/mise/`.cargo/**` in
    all rust + docker + policy watches (feature unification H3, version
    embedding H7, build-script identity H1). Bun/docs/opentofu skip on
    Rust-only diffs is SOUND.
- Codify broad-impact now: root `[lints]`/rustflags edits = all rust
    units (no such vector exists today; the rule must predate it).
- `Dockerfile` + `docker/build-mise.*` edits also run at least
    `rust-velnor-workflow` (all jobs download its runtime).
- Introduce prerequisite-vs-target tiers: dependency-pulled units run
    build-only `cargo check`, full fmt+clippy+nextest only for directly
    matched units + dependents. (Single-crate PR: ~7 full suites → 2–3.)

D3. Cache (budgets + behavior):

- Immediately: delete the 7 orphaned per-commit mbx entries (~2.14 GiB,
    suffix `-a1e07a28…`) → usage back under 10 GiB:
    `gh cache delete <id> --repo tailrocks/velnor` per orphan id.
    Account for Docker buildkit (~4.1 GiB) + 10 stable mbx entries
    (~5.1 GiB) in a standing budget; alert on >8 GiB.
- PR restore-only is UPSTREAM-DESIGNED (not a bug): warmth flows main →
    PR via prefix `restore-keys`. After orphans are gone, prove one warm
    PR run (≥90% restores, zero `Downloading crates`) before any other
    cache redesign.
- Fix mise 17s/job (pre-bake/pin toolchain or repair mise cache),
    cache nextest+mold binaries (keyed by version), keep runtime
    prebuilt-download direction for `setup-velnor-workflow`.
- Docker PR: scoped `--cache-to` write path or main-only release build
    (PR validates Dockerfile cheaply). PR must stop full-recompiling
    `Dockerfile:50-54` per `.rs` change.

D4. Topology + queue:

- Unwedge `velnor-target-mvp` (scale/fix runners) or move Velnor jobs to
    a non-gating workflow: 15 forever-queued jobs hold main + Nightly
    pending indefinitely.
- Nightly: narrow scope, add `ci-required`-equivalent + alerting (red
    blocks nobody today).
- Kill dual codegen per job (shared check artifacts or single
    `check --tests`), then shard the R5 slow tests with nextest
    partitioning.

D5. Safety nets that MUST exist before narrowing (status):

- Main full runs: EXISTS (forced full on push). Keep.
- Nightly full + alerting: ADD (see D4).
- `merge_group` full: MISSING — D1 one-liner, merge-blocking.
- Single resolution + no silent no-op: MISSING — D1, merge-blocking.
- Version-bump allowlist (unit + dependents; lockfile stays broad):
    ADD with fixture tests.
- Production-topology gate (`cargo check --workspace --all-targets`
    default features + release-feature boundary from `mise.toml:42-52`):
    wire into CI; per-crate `--all-features` green is not shippable (H2).
- `include_str!` coverage test: ADD (see D2).

## 5. Plan (in order, smallest correct change first)

P1. D3 orphans + budget proof; one warm PR run (≥90% restores). Unblocks
    all measurement.
P2. D1 merge-blocking safety (merge_group full, single resolution,
    empty-diff pass, three-dot diff) with fixture tests.
P3. D2 watch narrowing (docker, generator classification, docs, missing
    watches, file edges) — release-bump-equivalent diff must select ≤4
    units; single-crate PR selects target + closure only.
P4. D2 prerequisite build-only tier; D4 dual-codegen kill.
P5. D3 setup overhead (mise/nextest/mold) + Docker PR write path.
P6. D5 gates (production-topology, version allowlist, include_str! test),
    R5 test sharding, R6/R7 queue + nightly.
P7. Bootstrap hardening (prebuilt runtime; R1f).

Each step: generator change + regenerated workflows + regression test /
snapshot + one measured CI run. Never disable a check to gain speed.

## 6. What NOT to do

- No `MBX_DISABLE=1` / plain-cargo fallbacks as a "fix".
- No per-commit cache-key suffixes (re-floods the 10 GiB budget; the
  orphans in R1b are the corpse of that design).
- No "workflow unit only" generator narrowing (runtime executes every
  job; §4 D2 classification instead).
- No version/lockfile-only = no-op (version strings compiled in; H7).
- No touching the unmatched-file → full fallback.
- No claiming cache warmth from a single fast run: distinguish cold,
  prefix-restore, exact-hit, queue time, provider limits.
- No process theater: subagent counts, model names, report length are not
  progress. Only merged generator changes plus measured runs count.

## 7. What changed in this revision (v3)

Deep analysis 2026-09-06: 6 parallel investigators (scope logic, watch
graph, cache behavior, workflow topology, build/test cost, adversarial
review) + 2 independent verifiers re-checking every load-bearing claim
against code lines, `gh` logs, and cache API. Result: 17 of 18 sampled
claims CONFIRMED verbatim (1 count corrected: 12 units watch `Cargo.lock`
— docker + policy + 10 rust — not 11). Prior v2 root-cause list kept where
confirmed; watch-level precision, file:line anchors, and the D1–D5 target
configuration are new. All evidence re-derivable via §2 commands.

## 8. Acceptance (all required, none optional)

1. Before/after table using §2 protocol on comparable PRs: per-job
   totals, run-step splits, cache account lines.
2. Warm affected-scope PR within §1 SLOs, ≥90% Rust jobs restored.
3. `Cargo.lock`-only bump selects the minimal set (P3 fixture test);
   single-crate PR never runs unaffected units.
4. Cache account under budget with headroom; no warm-entry eviction in
   7 days; orphans deleted.
5. D5 safety nets (merge_group full, single resolution, nightly
   alerting, production-topology gate) merged before scope narrowing
   counts as done.
6. Remaining costs listed with proof they are intrinsic (provider,
   queue, required coverage), not just "slow".
7. Any unmet item stated exactly with blocking evidence — never
   "achieved".

## 9. Context (not work items)

- PR #557 (runner architecture merge): historical baseline only.
- PR #570 (Velnor sole generator), #572/#577 (source pins): current
  architecture; generator owns all workflow behavior.
- PR #580 (closed): stabilized mbx keys; live but insufficient (R1c).
- Mr. Boxington 1.8.3 default on both lanes; GitHub lane
  `backend: github`, Velnor lane `backend: local`. Version bumps must
  re-baseline §2.
