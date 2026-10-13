# CI/CD performance goal (v4)

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

## 0. Gate contract (what actually gates merge — verified 2026-09-06)

Required checks on `main` (ruleset 19573071): `DCO` + `ci-required` only.
`strict` (require-branches-up-to-date) is FALSE: stale-green merges are
legal today. Reviews required: 0. No merge queue exists (API 404), so the
`merge_group` trigger in `ci-pr.yml` never fires — any `merge_group`
prescription below is dead code until a queue is enabled (or the trigger
is deleted). `ci-required` needs plan + all 15 group-unit callers;
skipped-unselected counts as pass. Re-verify via
`gh api repos/tailrocks/velnor/rulesets/19573071 --jq .rules`.
All lanes are `ubuntu-24.04` (+ self-hosted Velnor); no macOS/Windows.

## 1. SLOs

| Metric | Baseline 2026-09-06 | Target |
| --- | --- | --- |
| PR gate wall (plan start → `ci-required` green), affected scope | 12m05s cold-cache (run 34022845304, EXPIRED — see below) / 12m09s warm-cache (run 34026137664) | ≤ 6m warm, net of queue |
| Slowest Rust PR job (`velnor-runner`) | 11m39s cold → 2m35s warm (exact GHA restore; dev 19s + test 52s) | ≤ 5m warm |
| Docker PR job | 9m18s → 11m46s (NOW the critical path) | ≤ 4m |
| Rust setup overhead per job (mise + nextest + mold) | ~25–30s after cache hit | ≤ 10s |
| PR jobs with usable build-cache restore | cold 08:47 → exact-key GHA hits from ~09:54 (mbx layer still 0-hit prefix restores, 5–10× faster compiles) | ≥ 90% restores, exact vs prefix reported separately |
| Main push wall (no queue) | ~25m, plus queue pile-ups | ≤ 15m |
| Nightly wall | 1h33m (run 34020370896, wedged) | measure, then budget |

08:47 cold-cache baselines EXPIRED 2026-09-06T09:53Z on merge of #582
(static Rust lanes through mbx; new `velnor-static-mbx-*` keys, changed
tree). Current reference: warm run 34026137664 (09:58:57→10:11:06).
Baselines expire on ANY merge touching `crates/velnor-workflow/**`,
`.github/**`, `Dockerfile`, `mise.toml`, or mbx version — re-baseline on
expiry, never compare across it.

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

Compilation is 94–99.5% of the slow Rust jobs when cold. Cache account
(10:2xZ direct check): 108 entries, 11.88 GiB vs ~10 GiB limit — still
over budget. The 7 legacy per-commit orphans (suffix `-a1e07a28…`,
last_access stuck 09:05) are STILL present — do not trust secondhand
"orphans gone" claims; re-list before deleting. Stable entries' last_access
is fresh (10:22Z restores happening). Never quote byte counts as literals
without a date; use the §2 `gh api` query + snapshot date.

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
  c. Every release-bump PR changes `Cargo.lock`, which sits in 12 units'
     watch lists but only 11 `hashFiles` key sets (docker has no mbx key —
     buildx layer instead): docker, rust-policy, all 10 other rust units —
     exact misses on the most common PR type. Prior attempt PR #580
     (closed; key stabilization) is live and did NOT fix wall time.
     (Count corrected by verifier: watch-12, keys-11. #582 added further
     `velnor-static-mbx-*` `Cargo.lock`-keyed static entries.)
  d. `mise install --locked rust` re-downloads 6 toolchain components
     (~17s) on EVERY Rust job despite the mise cache hit.
  e. nextest + mold binaries download on every job, uncached (~1–2s × 10).
  f. `setup-velnor-workflow` miss path compiles the runtime from source
     (~3–6 min); dormant today (cache hits) but one rev-bump from firing.

R2 — `affected` selects ~everything. PR #581 ran all 15 units. Verified:
  a. `crates/velnor-workflow/**` is watched by ALL 15 units (generator
     `unit.watch.push("crates/velnor-workflow/**")`, currently `lib.rs:2439`
     — line drifts, anchor on the symbol; `project.toml` watch lines
     38,52,63,77,91,105,119,134,149,164,178,193,208,223,237). Any generator
     change = full CI without ever hitting a fallback.
  b. Docker watches `crates/**` + `tools/**` (`project.toml:52`): ANY crate
     file (source, test, README, fixture) rebuilds the image.
  c. `Cargo.lock` + root `Cargo.toml` watched by 12 units; a version bump
     (own manifest + lockfile + changelog) selects 13/15 (all but bun and
     opentofu; changelog `*.md` pulls docs via `**/*.md`).
  d. Docs unit watches repo-wide `**/*.md` plus nonexistent `docs/**` and
     `mkdocs.yml`; real docs are `content/docs/**/*.mdx` (23 files — count
     corrected) which match NOTHING → full fallback. Crate READMEs trigger
     markdownlint noise; real doc edits trigger full CI. Both wrong.
  e. Opentofu `**/*.tf` matches exactly one file: a test fixture
     (`crates/velnor-workflow/tests/fixtures/polyglot/infra.tf`).
  f. Single-crate source change (e.g. `velnor-client/src/*`) correctly
     expands via dependents+prerequisites to ~7 rust units + docker — but
     every expanded unit runs the FULL suite (no build-only tier), and
     shared crates recompile per job (R3).
  g. Degrade-to-full triggers in `runtime.rs`: empty/unset/zero BASE_SHA,
     git-diff failure, any `.github/` path, any unmatched file
     (`fleet/**`, `schemas/**`, `docker/job-*` match nothing → full;
     `debian/` does not exist — do not cite it), empty diff hard-ERRORS
     (fails plan on no-op PRs), two-dot diff with possibly stale base
     over-selects, and unit jobs re-resolve on shallow checkouts (no
     `fetch-depth` in reusable workflows) so re-resolution silently
     degrades to full every time.

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
review, all confirmed) + ONE proven toolchain incompatibility:

R8a. mbx vs zig C-shim (PROVEN failure, scoped fix verified).
`velnor-runner` and `velnorctl` pull C code via
`aws-lc-rs ← sigstore-sign` (`cargo tree -i aws-lc-rs`; tools/workflow do
NOT). Release v0.1.264 amd64 log (run 34027800870, job 101474103309):
direct-cargo `zigbuild -p velnor-runner` linked OK, then `mbx zigbuild -p
velnorctl` failed at link: `ld.lld: error: undefined symbol:
__isoc23_sscanf ... bcm.c ... libaws_lc_sys-6f1fcba62f591763.rlib`.
Mechanism: objects built under mbx's wrappers reference glibc ≥2.38
fortified symbols the zig-0.16 bundled-older sysroot cannot resolve;
host==target only (aarch64 cross passed). History: `b7fac245` (#581)
`MBX_DISABLE=1 mbx zigbuild` → #583/#586 plain-cargo attempts (closed) →
rename to `VELNOR_DIRECT_CARGO=1 cargo zigbuild` (generator guard
`DIRECT_CARGO_ENV`, `lib.rs:2586`, template comment quoting this failure;
tests pin it at `lib.rs:8648,8751-8775`) → #589 enforced all-mbx on main
(test asserts no `cargo zigbuild`) → #590 re-added the direct bypass → #591
replaced it with `MBX_CC=0 mbx zigbuild` for the two aws-lc binaries. The
fix disables only mbx's C/C++ build-script shim; Rust remains inside mbx and
all other release packages stay on normal mbx. Pinned release run 34031576987
passed both amd64 and arm64 binary/deb jobs with no `__isoc23_sscanf` failure.
Never expand the scope to other packages; never claim the C shim is enabled
for those two invocations.

R8b. Manifest-invisible file couplings (all confirmed): `velnor-tools`
`include_str!`s `../../velnor-runner/src/manifest.rs` in a TEST while depending only on
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

- Single resolution, stated as OUTCOME (implementer picks cheapest
    measured mechanism): a unit job performs ZERO git reads — plan emits
    the unit list once, jobs consume it verbatim. Any SHA/artifact
    mismatch fails closed (non-zero exit) with both SHAs in a
    `::warning::`. Fixture: shallow unit checkout + valid artifact still
    runs the correct subset. (Rationale: "pass allowlist through" alone
    leaves the second git read + shallow-degrade path alive.)
- `merge_group` → `full` counts ONLY together with an enabled merge
    queue (see §0: trigger is dead today). EITHER enable the queue and
    prove queued combos test together, OR delete the trigger. The
    one-liner alone counts as nothing.
- Empty diff → empty selection (vacuous pass), not `Err`.
- Stale-base over-selection: OUTCOME "stale-base fixture does not
    over-select" + passing fixture; diff syntax (two- vs three-dot) is
    implementer's choice.
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
- Add missing watches: `schemas/**` → model+dependents; `fleet/**` →
    tools+runner; `microvm/**` → ctl; `velnor-tools` +=
    `crates/velnor-runner/src/manifest.rs` (`include_str!` edge). Teach the
    generator to parse `include_str!` and fail when a target is
    watch-uncovered. (No `debian/` dir exists — no watch needed.)
- NOTE — atomicity: until ALL missing watches land in ONE generator
    change + regen + fixture matrix, partial steps still fall to full via
    the (correct, untouchable) unmatched fallback and measure zero gain.
    P3 below is therefore atomic, with fixture-only acceptance per
    sub-step and ONE measured CI run after the atom lands.
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
- Docker PR, stated as OUTCOME: PR Docker job ≤4m; an `.rs`-only change
    never recompiles the release layer (`Dockerfile` COPY/mbx-build
    block); `Dockerfile`/build-mise edits still validate the image.
    Guardrail: `release.yml` verify-tag + full-scope main run green.
    (Main-only release builds would widen the R8 build.rs-identity hole —
    rejected unless that gate moves into PR.)

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

## 5. Plan (in order; later steps may not assume earlier ones)

P1. D3 orphans (re-list first — direct check 10:2xZ says present; one
    auditor misreported deletion) + budget ≤8 GiB proof; warm-PR evidence
    (already emerging: exact GHA hits from ~09:54 — consolidate, don't
    re-prove from zero).
P2. D1 merge-blocking safety (queue-or-delete-trigger, single resolution
    as OUTCOME, empty-diff pass, stale-base fixture) + version-bump
    allowlist fixture test FIRST (it is what reconciles KEEP-lockfile
    with a small lock-bump selection; without it P3's "≤4 units" is
    unprovable — restate as "lock-bump selects exactly the enumerated
    allowlist set, hypothesized ≤4, must be proven").
P3. ATOMIC D2 watch landing (docker, generator classification, docs,
    missing watches, file edges) + regen + fixture matrix; single-crate
    PR selects target + closure only. Fixture-only acceptance per
    sub-step; ONE measured CI run after the atom.
P4. D2 prerequisite build-only tier, stated as OUTCOME (≤3 full suites on
    a single-crate PR, dependents still compile; workspace
    `cargo check --workspace --all-targets` default-features gate green);
    D4 dual-codegen kill.
P5. D3 setup overhead (mise/nextest/mold) + Docker PR outcome (D3).
P6. D5 gates (production-topology with named workflow + command,
    include_str! test), R5 test sharding, R6/R7 queue + nightly (named
    red-to-signal path + one test red run).
P7. Bootstrap hardening (prebuilt runtime; R1f).

Each step: generator change + regenerated workflows + regression test /
snapshot + one measured CI run. Never disable a check to gain speed.

## 6. What NOT to do

- No `MBX_DISABLE=1`, `VELNOR_DIRECT_CARGO=1`, or any namespaced-env
    plain-cargo carve-out as a "fix" FOR PR CI LANES. (The #588 branch
    renames the bypass without removing it — renames do not comply.)
    RELEASE FIX (proven, narrow): use `MBX_CC=0 mbx zigbuild` for the
    aws-lc-carrying bins (`velnor-runner`, `velnorctl`) — see R8a. This
    keeps the Rust builds behind mbx while disabling only the C/C++ shim.
    Keep it scoped to those two invocations and retain the failure-signature
    comment. #588's body claim "All Rust lanes remain behind mbx" was FALSE
    while the direct Cargo bypass existed; #591 restored that Rust path.
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

## 7. What changed in this revision (v4)

Re-verification 2026-09-06 ~10:20Z: 4 more subagents (goal-accuracy audit,
open-PRs analysis, CI re-measurement, adversarial critique).

- Baselines: §1 cold numbers reconfirmed to ±9s, but EXPIRED by #582
  (09:53Z); new warm reference run 34026137664. Critical path flipped
  Rust → Docker (11m46s). Rust warm via exact GHA hits (dev ~19s).
- Selection model VALIDATED on live data: predicted 9-unit set for #588's
  2-file state matched run 34026985573 exactly (9/9, no over/under).
- Corrections applied: .mdx 23 (was 20), `lib.rs:2439` (was 2434), watch-12
  vs keys-11, `debian/` vacuous, §0 gate contract added (required: DCO +
  `ci-required`; `strict=false`; no queue → merge_group dead), §6 ban
  generalized past the `VELNOR_DIRECT_CARGO` rename, plan reordered
  (allowlist before ≤4; atomic P3), mechanisms restated as outcomes
  where the goal over-prescribed.
- One auditor misreported orphans as deleted; direct `gh cache list`
  check overruled it (still present 10:2xZ). Trust commands, not reports
  — including this file. Re-derive via §2.
- Open PR #588 (9 files, full-scope): runner FAILED on
  `supervised_controller_capacity…` (flaky, NOT the stabilized cancel
  test); Docker slowest again. Blocked on that failure, not on the goal.

## 8. Acceptance (all required, none optional)

1. Before/after table using §2 protocol on comparable PRs, where
   comparable = same unit-selection output + same base tree modulo the
   change + same cache-generation keys; queue time reported separately
   (SLOs are net of queue).
2. Warm affected-scope PR within §1 SLOs: exact-hit and prefix-restore
   rates reported SEPARATELY (≥90% combined restores; exact-hit rate
   tracked as its own metric).
3. `Cargo.lock`-only bump selects exactly the P2-allowlist enumerated set
   (unit ids listed in the fixture); single-crate PR runs target +
   closure ids listed, never others. "Minimal" without ids proves nothing.
4. Cache: 7-day daily `gh api .../actions/caches --paginate --jq` snapshot
   log; headroom = ≤8 GiB (not ~10); orphans deleted (re-list to confirm).
5. D5 safety nets merged before scope narrowing counts: queue-or-delete
   trigger proof, single-resolution fixture, nightly named red-to-signal
   path + one test red run, production-topology named workflow + command.
6. Remaining costs listed with proof they are intrinsic (provider docs
   link + queue timestamps + coverage requirement), not just "slow".
   Provider/queue/coverage cited without those artifacts do not count.
7. Any unmet item stated exactly with blocking evidence — never
   "achieved". Confession does not count as completion either: externally
   blocked items (e.g. R6 runner capacity) stay OPEN with owner + date.

## 8a. Live evidence log (2026-09-06)

Status remains `NOT ACHIEVED`.

### Comparable timing and topology

| Run | Selection | Queue | Wall | Result |
| --- | --- | ---: | ---: | --- |
| [34026137664](https://github.com/tailrocks/velnor/actions/runs/34026137664) | warm affected GitHub baseline | 3s (09:58:57→09:59:00Z) | 12m09s (09:58:57→10:11:06Z) | green |
| [34042229133](https://github.com/tailrocks/velnor/actions/runs/34042229133) | affected output, but generator diff made all 16 units full | 3s (15:24:55→15:24:58Z) | 13m08s | green; not comparable after |
| [34044939215](https://github.com/tailrocks/velnor/actions/runs/34044939215) | main full run after v0.1.270 | 4m28s (16:17:01→16:21:29Z) | 21m12s (→16:38:13Z) | red; old Velnor image/runtime contract |

No comparable post-change warm affected PR exists yet. Queue is reported
separately; SLO comparison remains open.

PR [#602](https://github.com/tailrocks/velnor/pull/602) keeps GitHub and Velnor
callers in the same generated reusable workflows. The source fix makes setup
actions lane-specific: Velnor keeps checkout, artifact download, cache restore,
and local Mr. Boxington only; the image owns Bun, OpenTofu, Rust tools, mold,
and Buildx. Generated output has no `ci-velnor.yml` split.

The failed main run is diagnostic evidence, not a passing measurement:

- Velnor policy rejected `taiki-e/install-action` (job `101519120940`).
- Velnor OpenTofu rejected `opentofu/setup-opentofu` (job `101519121014`).
- Velnor Bun rejected `oven-sh/setup-bun` (job `101519121223`).
- Velnor Docker reached `docker/setup-buildx-action` and failed to connect to
  `/var/run/docker.sock` (job `101519120966`).
- Velnor Documentation executed, then the deployed image binary rejected
  generated `version_bump_units` at `.github/ci/project.toml:19` (job
  `101519121135`). This proves the job-image `velnor-workflow` binary is older
  than the checked-in generator; a new image release is required before live
  Velnor proof.

Five Velnor runners are currently online and idle:
`velnor-dogfood-slot-1`, `-2`, `-3`, `-4-next-2695994-10`, and `-5` (runner
API checked 2026-09-06T16:46:14Z). Capacity is no longer the immediate blocker.

### Cache snapshots

| Captured UTC | Entries | Bytes | GiB | Orphan state |
| --- | ---: | ---: | ---: | --- |
| 2026-09-06T15:57:33Z | 11 | 5,488,621,339 | 5.112 | old tag/feature refs re-listed absent |
| 2026-09-06T16:46:14Z | 51 | 10,004,871,521 | 9.318 | no old tag/feature refs; main Docker/mbx caches present |

The 43 old `v0.1.268` entries and 52 synthetic feature-branch entries were
deleted by exact ID and re-listed absent. The current account exceeds the
8-GiB acceptance ceiling. The required seven consecutive daily snapshots are
not present.

### Safety fixtures and remaining blockers

- Commit `4df3da27` removes the blanket `src/lib.rs` watch edge. The checked
  configuration matches a generator edit to exactly `docker`,
  `rust-velnor-workflow`, and `rust-production-topology`; `runtime.rs` remains
  watched by every unit. This prevents generator-only edits from selecting all
  15 verification units while retaining the image, regeneration, and workspace
  safety gates.
- Local proof after that change: `cargo test --locked -p velnor-workflow` passed
  133 tests; `cargo check --workspace --all-targets --locked` passed; the
  generator `--check` passed; and `mise run test-release-feature-boundary`
  passed by observing the intended release-profile `test-support` rejection.
- The maintenance generator now uploads each cache snapshot before enforcing
  the 8-GiB limit. Over-budget days therefore remain observable as artifacts
  and still fail the budget gate.

- Cargo.lock allowlist IDs: `docker`, `rust-velnor-bench`,
  `rust-velnor-runner`, `rust-velnorctl`.
- Single-crate `velnor-client` full IDs:
  `docker`, `rust-velnor-client`, `rust-velnor-tools`, `rust-velnorctl`,
  `rust-production-topology`; prerequisite IDs:
  `rust-velnor-model`, `rust-velnor-control`, `rust-velnor-render`,
  `rust-velnor-runner`.
- Nightly synthetic red run
  [34043106625](https://github.com/tailrocks/velnor/actions/runs/34043106625)
  failed `nightly-required`, passed red-to-signal, opened issue #599, and the
  synthetic issue was closed during cleanup. Velnor jobs were correctly
  skipped on the feature ref.
- Production topology is generated as
  `ci-rust-production-topology.yml`; its Velnor command is
  `mbx check --workspace --all-targets --locked` plus
  `mise run test-release-feature-boundary`.
- Remaining unmet items: PR #602 CI completion, new job image
  release/deployment, live passing Velnor main proof, comparable warm affected
  PR, exact/prefix restore rates after the fix, Docker ≤4m proof, seven-day
  ≤8-GiB snapshots, and the required provider/queue/coverage proof table. Do
  not claim achievement.

## 9. Context (changelog, not work items)

- #557 runner-arch merge: historical baseline only.
- #570 Velnor sole generator; #572/#577 source pins: current arch.
- #579 MERGED (release admission races).
- #580 CLOSED: stabilized mbx keys; live but insufficient (R1c).
- #581 MERGED (v0.1.263): the §2 cold-baseline run's own bump.
- #582 MERGED 09:53Z (static Rust lanes through mbx): EXPIRED all
  pre-09:53 baselines; added `velnor-static-mbx-*` keys.
- #583/#586 CLOSED (release-link MBX bypass attempts); local revert chain
  keeps the runner release link off-MBX under renamed env
  `VELNOR_DIRECT_CARGO=1` — non-compliant with §6 by rename.
- #584 CLOSED (v0.1.264 bump + drop-superseded).
- #585/#587 CLOSED (goal-doc churn, superseded); #588 MERGED (v4 text +
  cancel.rs test stabilization; runner flake
  `supervised_controller_capacity…` failing, Docker critical path).
- #589 MERGED (enforce all-mbx release: deleted `DIRECT_CARGO_ENV`,
  test asserts no `cargo zigbuild`); #590 MERGED (temporary direct
  cargo-zigbuild bypass for runner+ctl); #591 MERGED (replace that bypass
  with scoped `MBX_CC=0 mbx zigbuild`, validated by release run 34031576987).
- Staleness policy: every measurement dated; expires after 7d or any
  generator/`.github`/Docker/mise/mbx change, whichever first. Anchors
  are symbol/pattern-first (`scope_for_event_values`,
  `git_changed_files`, `unit.watch.push("crates/velnor-workflow/**")`,
  unit ids + glob strings); line numbers informational + dated.
