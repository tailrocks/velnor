# CI/CD performance goal (v2)

Status: NOT ACHIEVED. Previous revision let agents claim success without
measurements. This revision is falsifiable: no claim counts without the
before/after table in §7.

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
| PR jobs with usable build-cache restore | 0 of 15 (all `No mbx cache found`) | ≥ 90% prefix restores |
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

| Job | Total | Run step | Cache |
| --- | --- | --- | --- |
| velnor-runner (101458506956) | 11m39s | 11m10s (1685 tests, 35s test time) | cold |
| velnorctl (101458506934) | 10m43s | 10m07s (dev 4m51s + test 5m11s) | cold |
| velnor-bench (101458507063) | 9m00s | 8m28s (dev 4m04s + test 4m19s) | cold |
| Docker (101458506866) | 9m18s | 8m54s, incl. 7m46s release compile | Dockerfile cache missed |
| velnor-tools (101458506938) | 4m39s | 4m14s | cold |
| velnor-workflow / control / render / client / model / unit-collector | 53s–1m56s | 25–82s | cold |
| bun / docs / opentofu / policy | 9–49s | — | ok |

## 3. Root causes (ranked, evidence-backed)

R1 — Build cache never warm on PRs. All 15 PR jobs cold on 2026-09-06
(`0 B downloaded`, `first build`). Contributing factors, each must be
verified and fixed, not assumed:
  a. `save-on-workflow-dispatch: true` semantics in generated mbx setup
     (`lib.rs` ~L4327): prove which events save. PR jobs must restore AND
     save (subject to fork-PR restrictions), main must save.
  b. GitHub 10 GiB account cache budget vs ~4.5 GiB Rust per-unit entries
     (224 MiB–1.22 GiB each, 2026-09-06 listing) plus Docker buildkit blobs:
     prove eviction is not wiping warm entries; budget per scope.
  c. Every release-bump PR changes `Cargo.lock`, which is in every unit's
     `hashFiles` key: exact misses on the most common PR type. Prefix
     `restore-keys` must demonstrably warm these (it did not on 09-06).
  d. Prior attempt PR #580 (closed, key stabilization) is already live in
     the generator and did NOT fix wall time: key stability alone is
     insufficient. Do not re-litigate #580; fix the remaining a–c.

R2 — `affected` scope selects ~everything. PR #581 ran all 15 units.
Causes: any `.github/` change forces full (`runtime.rs` L650); every unit
watches `crates/velnor-workflow/**` (generator L2434) and `Cargo.lock`, so
generator or release PRs match all units; any unmatched file forces full
(L676). Fix: narrow watches (e.g. generator changes select the workflow
unit + a workflow-validation job, not all Rust units), version-bump-only
PRs must select a minimal set, and log the selected-unit reason per run.

R3 — Shared dependencies compiled once per unit. `velnor-model` etc. are
rebuilt inside every dependent's isolated per-unit cache; each job also
builds twice (clippy dev profile + nextest test profile: velnorctl
4m51s + 5m11s). Fix direction: shared dependency cache layer or merged
`--all-targets` single invocation; measure duplicate crate-compile seconds
before choosing.

R4 — Per-job setup overhead ×15 jobs: mise cache hits yet still
`mise install --locked rust` downloads 6 toolchain components (~17s);
`taiki-e/install-action` fetches nextest and mold binaries every job;
plan job builds/uploads a 2 MiB runtime artifact every unit downloads.
Pin/cache toolchain hermetically or move to prebuilt image tools.

R5 — Docker PR job rebuilds release binaries (7m46s `mbx build --release`
of 4 binaries). PR uses `--cache-from` only, never `--cache-to`
(`github_docker_cache_command`, `lib.rs` L2461); `COPY crates ./crates`
invalidates everything on any Rust change. Fix: PR-scoped cache write or
stop building release artifacts on PRs (validate Dockerfile cheaply;
build images on main only).

R6 — Slow tests dominate warm time even after caching: runner suite 1685
tests with single outliers (`idle_resource_scaling…` 20.1s,
`oauth_client_assertion…` 11.9s, `supervised_controller…` 8.3s,
`complete_job_retries_5xx…` 5.1s). Split or parallelize the slowest
integration tests with nextest partitioning; budget per-crate test time.

R7 — Main queue serialization: `ci-main` concurrency
`cancel-in-progress: false` piles pushes (runs observed queued 25–28m,
nightly 1h33m). Fix: merge-queue discipline and/or narrower main scope,
nightly budget with sharding.

R8 — Plan/runtime bootstrap fragility: `setup-velnor-workflow` falls back
to `cargo install --git … --rev …` (full source compile, minutes) on
cache miss, and plan uses `fetch-depth: 0`. Prefer immutable prebuilt
runtime download; shallow-fetch where history is unneeded.

## 4. Plan (in order, smallest correct change first)

P1. Reproduce cold cache on demand: document exact save/restore behavior
    per event for mbx, mise, Docker GHA cache. One table, log excerpts.
P2. Make one warm PR run exist: fix R1a–c, then show a PR run where ≥90%
    of Rust jobs log a cache restore and zero `Downloading crates`.
P3. Fix affected-scope over-selection (R2): release-bump fixture test +
    generator change; PR #581-equivalent diff must select ≤3 units.
P4. Remove duplicate compilation (R3): measure shared-crate recompile
    seconds across jobs, implement one dedup mechanism, re-measure.
P5. Cut per-job setup (R4) and Docker PR rebuild (R5).
P6. Budget tests (R6) and main/nightly queue (R7).
P7. Harden bootstrap (R8).

Each step: generator change + regenerated workflows + regression test /
snapshot + one measured CI run. Never disable a check to gain speed.

## 5. Per-change requirements

- Broad-impact classification (workspace manifests, lockfiles, toolchain,
  build scripts, generator, CI files) stays conservative; narrowing R2
  must add fixture tests proving adversarial cases still select full.
- Cache changes must prove correctness: key inputs cover OS, arch,
  toolchain, target, features, lockfiles; no false-hit reuse across
  dependency changes.
- Dependents/prerequisites closure (`expand_affected_units`) semantics
  unchanged unless §7 explicitly re-baselined.

## 6. What NOT to do

- No `MBX_DISABLE=1` / plain-cargo fallbacks as a "fix".
- No per-commit cache-key suffixes (re-floods the 10 GiB budget; see R1b
  and the comment at `lib.rs` L4321).
- No claiming cache warmth from a single fast run: distinguish cold run,
  prefix-restore run, exact-hit run, queue time, and provider limits.
- No process theater: subagent counts, model names, and report length are
  not progress. Only merged generator changes plus measured runs count.

## 7. Acceptance (all required, none optional)

1. Before/after table using §2 protocol on comparable PRs (same scope
   class): per-job totals, run-step times, cache account lines.
2. A warm affected-scope PR finishes per §1 SLOs, with logs showing
   restores (no `No mbx cache found`) on ≥90% of Rust jobs.
3. A `Cargo.lock`-only bump PR selects the minimal unit set (P3 test).
4. Cache account: total `velnor-mbx-*` + Docker GHA entries stay under
   budget with headroom; no warm-entry eviction within 7 days.
5. Remaining costs listed with proof they are intrinsic (provider,
   queue, or required coverage), not just "slow".
6. If any item is unmet, the conclusion states exactly which, with the
   blocking evidence — never "achieved".

## 8. Context (not work items)

- PR #557 (runner architecture merge): historical baseline only.
- PR #570 (Velnor sole generator), #572/#577 (source pins): current
  architecture; generator owns all workflow behavior.
- PR #580 (closed): stabilized mbx keys; live but insufficient (see R1d).
- Mr. Boxington 1.8.3 is the default on both lanes; Velnor lane uses
  `backend: local`. Version bumps must re-baseline §2.
