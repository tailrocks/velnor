# Hygiene task-extraction plan (jackin PR #994 → Velnor check_profiles)

READ-ONLY plan. No repo file was modified to produce it.

## 1. Evidence pins

- Jackin checkout `/Users/donbeave/Projects/jackin-project/jackin`, main `0be3fcf9`.
- PR ref `origin/pr/994` (head `ab5b0c4e`):
  `.github-gen/sources/workflows/hygiene.yml`, 1260 lines, 16 jobs.
- Velnor contract: `crates/velnor-workflow/src/config/mod.rs`
  (`valid_check_profile_task`, `validate_check_profile`) and
  `crates/velnor-workflow/src/primitives/check_profiles.rs`
  (`render_profile_job`, `render_tool_steps`).
- Jackin task layout on main: `mise.toml` inline `[tasks.*]` (no `mise-tasks/`
  dir); Rust logic in `crates/jackin-xtask` invoked as `cargo xtask <sub>`.

## 2. Contract the tasks must satisfy

- `tasks` entries render verbatim as `run: mise run {task}` (one step each,
  in order). Charset (`valid_check_profile_task`): first byte alnum or
  `_ . : / +`; rest alnum or `- _ . : / +`. No whitespace, no shell.
  All names below use `hygiene-[a-z0-9-]*` and pass.
- `status` is `required` (default) or `advisory` (= `continue-on-error: true`).
- `tools` must name mise-lock keys (`validate_check_profile_tools`); mirror
  each job's `install_args`. Tool steps install **only** mise tools — no
  Mr. Boxington/`mbx`, no mold, no sccache, no rustup cache, no `$CI_XTASK`
  download, no cargo-target cache (see §6 gaps).
- `env` is static strings only (thresholds via env work, e.g. `MIRIFLAGS`).
- No matrix, no dispatch inputs, no cross-run artifact download, no per-step
  `continue-on-error`, one artifact upload (`if-no-files-found: warn`, single
  name = profile id) per profile.

## 3. Existing homes for the logic

- Mise tasks on main: only `construct-*`, `build/ci/test/lint/fmt`,
  `desktop-*`, `swift-package-native-ci`. **Zero** hygiene-adjacent tasks.
- xtask subcommands already covering hygiene ground (reuse, do not duplicate):
  `ci-fuzz --package` (exact 4-crate/6-target contract the scheduled long fuzz
  runs repeat), `ci-build-times`, `lint ratchet --only`, `telemetry-bench
  --capture`, `frame-timing`, `health --format json`, `ci --only e2e`.
- Convention: mise `[tasks.*]` entries are thin entry points; substantive logic
  lives in `cargo xtask`. Small bodies (<~15 lines, straight-line) may stay
  inline shell in `mise.toml` (precedent: `swift-package-native-ci`,
  `desktop-verify`).

## 4. Per-job plan

| # | Job (line) | Inline shell purpose (1 line) | Extract? | Proposed task(s) → home |
|---|------------|-------------------------------|----------|--------------------------|
| 1 | `cache-usage` (31) | gh API cache-usage + top-10 table into summary | **NO** — platform reporting, no product assertion | — (stays workflow/generator maintenance) |
| 2 | `scheduled-hygiene` (63) | deny + hack + shellcheck + 6 long fuzz runs | YES | 7 tasks → `mise.toml` (+ fix `ci-fuzz`) |
| 3 | `native-macos` (166) | macOS release build + workspace nextest smoke | YES | 2 tasks → `mise.toml`, runner `macos` |
| 4 | `bench-run` (232) | compile + 4 criterion benches + soak + baseline capture | YES | 7 tasks → `mise.toml` + existing `telemetry-bench` |
| 5 | `dhat-allocation` (333) | dhat-heap allocation nextest suite | YES | 1 task → `mise.toml` |
| 6 | `cold-start-bench` (403) | hyperfine cold-start + PTY frame timing | YES | 2 tasks → `mise.toml` + new xtask `cold-start` |
| 7 | `rust-analyzer-clean` (511) | analysis-stats + fail on `^error` lines | YES | 1 task → `mise.toml` (inline) |
| 8 | `build-time-measure` (573) | measure clean/incremental + enforce ceilings | YES | 2 tasks → `mise.toml` → existing xtask, runner `github` |
| 9 | `beta-clippy-canary` (638) | beta clippy + top-20 warning summary | YES | 1 task → `mise.toml` (inline), advisory |
| 10 | `coverage` (688) | llvm-cov nextest on 5 pure crates → lcov | YES | 1 task → `mise.toml` (inline), advisory |
| 11 | `miri` (752) | nightly miri matrix over 3 pure crates | YES | 1 looping task → new xtask `hygiene miri`, advisory |
| 12 | `mutants` (815) | cargo-mutants on manifest+config, never fails | YES | 1 task → `mise.toml` (inline), advisory |
| 13 | `hakari-timing` (881) | before/after workspace-hack wall-clock | **NO** — investigation decided (No-adopt, run 29397057453, 152s/152s) | — (drop profile; decision lives in rust-tooling/index.mdx:161) |
| 14 | `dylint-advisory` (1027) | custom dylint render-purity lint, status propagated | YES | 1 task → `mise.toml` (inline), **required** (preserve) |
| 15 | `dind-chaos` (1102) | docker preflight + full DinD E2E suite | YES | 2 tasks → `mise.toml` → existing `ci --only e2e`, runner `github` |
| 16 | `health-trend` (1171) | prior-snapshot history + health JSON envelope | PARTIAL | 1 task → new xtask `health --snapshot`; history-restore stays out (no primitive) |

### Job 1 — `cache-usage` → DO NOT EXTRACT
Evidence: body is only `gh api repos/…/actions/cache/usage`,
per-cache `gh api …/actions/caches`, `jq`, `awk` into `$GITHUB_STEP_SUMMARY`.
No repo code, no threshold, no pass/fail. It is GitHub-platform reporting, the
same domain as the generator's `[maintenance]`/`cache-plan` surface. Baking
`gh`-API coupling into a product `mise run` task inverts ownership. Keep as a
workflow-owned step or move to the maintenance primitive.

### Job 2 — `scheduled-hygiene` → profile `scheduled-hygiene` (timeout 60, required)
Six inline bodies → seven tasks (fuzz splits per crate for independent
`mise run` steps and per-crate timeouts):
- `hygiene-deny-advisories` — `mbx deny check advisories`. Threshold: advisories
  only (deliberately narrower than xtask policy's `advisories bans licenses
  sources`; keep the narrower scope, do not "upgrade" to `ci --only policy`).
- `hygiene-cargo-hack` — `cargo hack check --workspace --feature-powerset --all-targets --locked`.
- `hygiene-shellcheck` — `shellcheck docker/runtime/entrypoint.sh
  docker/runtime/agent-status/hooks/*/report-hook.sh scripts/*.sh` (file list is
  product-owned; task owns it).
- `hygiene-fuzz-config|manifest|env|protocol` — one per crate, each
  `cargo xtask ci-fuzz --package <crate> --max-total-time 120`.
  **Prerequisite structural fix:** `ci-fuzz` today hard-requires `CI_CARGO_FUZZ`
  (`ci_fuzz.rs`: "CI_CARGO_FUZZ must be set"), nothing in the repo sets it, and
  no workflow calls `ci-fuzz` — the subcommand is currently unrunnable, which is
  why the scheduled job hand-rolls raw `cargo fuzz`. Fix `ci-fuzz` to resolve
  the cargo-fuzz binary itself (PATH/`mise which`, env override kept), so the
  PR-short (default 5s) and scheduled-long (120s) runs share one contract
  (sanitizer none, `x86_64-unknown-linux-gnu`, same 6 targets) instead of
  drifting. Fuzz-cache path `crates/*/fuzz/target` also needs a generator-side
  cache answer (§6).
- Profile `tools`: `rust shellcheck cargo-binstall cargo:cargo-deny
  cargo:cargo-fuzz cargo:cargo-hack` (mirror of `install_args`).

### Job 3 — `native-macos` → profile `macos-smoke` (timeout 60, required, runner `macos`)
- `hygiene-macos-build` — `mbx build --release --locked` (see §6 `mbx` gap).
- `hygiene-macos-test` — `mbx nextest run --workspace --locked`.
- The `if: lanes != 'velnor'` dispatch gate has no check_profiles equivalent;
  macos profiles always schedule. Tools: `rust
  aqua:nextest-rs/nextest/cargo-nextest github:open-telemetry/weaver`.

### Job 4 — `bench-run` → profile `bench-run` (timeout 90, advisory)
Job comment requires one bench invocation per step (Velnor runner observed to
report success while later pipelines never executed); check_profiles' one-step-
per-task rendering preserves exactly this. Seven tasks:
- `hygiene-bench-compile` — `mbx bench --workspace --locked --no-run` (tee log).
- `hygiene-bench-telemetry|runtime|console|capsule` — `mbx bench -p
  <jackin-telemetry|jackin-runtime|jackin|jackin-capsule> --bench
  <disabled_fast_path|launch_pipeline|console_frame|pane_body> --locked -- --quick`.
  Bench list + `--quick` are product-owned; all four benches exist on main.
- `hygiene-bench-soak` — `mbx nextest run --profile soak -p jackin-telemetry -p
  jackin-diagnostics --all-features --locked --run-ignored ignored-only`.
- `hygiene-bench-capture` — `cargo xtask telemetry-bench --capture` (5% reviewed
  threshold + same-run calibration already owned by xtask; do not re-express).
- Artifacts: `bench-output.txt`, `target/telemetry-bench-current.json`,
  `target/criterion/**/estimates.json`. Tools: `rust
  aqua:nextest-rs/nextest/cargo-nextest`.

### Job 5 — `dhat-allocation` → profile `dhat-allocation` (timeout 45, advisory)
- `hygiene-dhat` — `mbx nextest run -p jackin-capsule --features dhat-heap
  --locked --no-capture` (tee `dhat-allocation.txt`).
- Static const budgets in `jackin-capsule/src/perf_budgets.rs` stay the ratchet
  source; task only runs the suite. Artifact: `dhat-allocation.txt`.

### Job 6 — `cold-start-bench` → profile `cold-start` (timeout 45, advisory)
- `hygiene-cold-start` — build release `jackin`, hyperfine
  (`--warmup 3 --min-runs 10`, commands `$bin --help`, `$bin console --help`,
  `--export-json cold-start.json`), plus the `/opt/mise` hyperfine-poison
  diagnose + `mise install --force hyperfine` workaround and the `--help`
  preflight. This is ~20 lines with branching — new xtask subcommand
  (`cargo xtask cold-start`) per the logic-in-Rust rule; mise task delegates.
- `hygiene-frame-timing` — existing `cargo xtask frame-timing --binary
  target/release/jackin --output frame-timing.json --samples 3` (threshold:
  samples 3; advisory maxes reported, not gated).
- Artifacts: `cold-start.json`, `frame-timing.json`. Tools: `rust hyperfine`.

### Job 7 — `rust-analyzer-clean` → profile `rust-analyzer` (timeout 45, advisory*)
- `hygiene-rust-analyzer` (mise inline shell): `rustup component add
  rust-analyzer`; `rust-analyzer analysis-stats .`; fail when output matches
  case-insensitive `^error`. Threshold = that grep; keep it in the task body.
- Artifact: `ra-stats.txt`. Tools: `rust`.
- \*Job has no job-level `continue-on-error` but its only failing step is
  `continue-on-error: true` → net advisory. Preserve as `advisory`.

### Job 8 — `build-time-measure` → profile `build-time` (timeout 90, required, runner `github`)
- `hygiene-build-times` — `cargo xtask ci-build-times` (existing).
- `hygiene-build-time-ratchet` — `cargo xtask lint ratchet --only build-time`
  (existing; ceilings live in `ratchet.toml`).
- **Runner must stay `github`**: calibration contract in the job comment —
  ceilings sampled on GitHub-hosted hardware; Velnor runners materially slower
  (run 32711045540: `jackin-runtime.clean_s` 220 → 322). A velnor-scheduled
  profile would force ceiling inflation or flapping.
- Artifact: `target/build-times.json`. Tools: `rust`. Note the `$CI_XTASK`
  prebuilt-xtask shortcut disappears (§6).

### Job 9 — `beta-clippy-canary` → profile `beta-clippy` (timeout 60, advisory)
- `hygiene-beta-clippy` (mise inline shell): `rustup toolchain install beta
  --profile minimal --component clippy`; `cargo +beta clippy --workspace
  --all-targets --all-features --locked` (never fails; `|| true`); top-20
  `^(warning|error)` count summary. No threshold by design (canary).
- Artifact: `beta-clippy.log`. Tools: `rust`.

### Job 10 — `coverage` → profile `coverage` (timeout 45, advisory)
- `hygiene-coverage` (mise inline shell): `rustup component add llvm-tools`;
  `cargo llvm-cov nextest -p jackin-core -p jackin-config -p jackin-manifest -p
  jackin-protocol -p jackin-env --lcov --output-path coverage.lcov` (+ summary
  report). Product-owned: the 5-crate list.
- Artifact: `coverage.lcov`. Tools: `rust cargo-binstall
  cargo:cargo-llvm-cov aqua:nextest-rs/nextest/cargo-nextest`.

### Job 11 — `miri` → profile `miri` (timeout 180, advisory)
check_profiles has no matrix. Three tasks in one profile would stop at the
first failure (no per-step `continue-on-error`), which is worse than the
matrix's independent invocations (job comment: one slow crate must not starve
the rest). So: **one** task owning the loop —
- `hygiene-miri` → new xtask `cargo xtask hygiene miri` (or `ci-miri`):
  `rustup toolchain install nightly --profile minimal --component miri`, then
  `cargo +nightly miri test -p <crate> --no-default-features --locked` for
  `jackin-core, jackin-config, jackin-manifest`, collecting per-crate
  PASS/FAIL and exiting nonzero iff any failed (advisory profile still reports
  red-but-tolerated, matching matrix semantics). `MIRIFLAGS=
  -Zmiri-disable-isolation` (tempdir tests) via profile `env` or task body.
- Tools: `rust`.

### Job 12 — `mutants` → profile `mutants` (timeout 90, advisory)
- `hygiene-mutants` (mise inline shell): `cargo mutants -p jackin-manifest -p
  jackin-config --timeout 120 --in-place -- --locked`, always `exit 0`, tail-40
  summary. Thresholds: crate list, `--timeout 120`, `--in-place`.
- Artifacts: `mutants.out/`, `mutants-summary.txt`. Tools: `rust cargo-binstall
  cargo:cargo-mutants`.

### Job 13 — `hakari-timing` → DO NOT EXTRACT
Evidence: the adopt/no-adopt question is decided — `docs/content/research/
engineering/ci/rust-tooling/index.mdx:161` records **No-adopt** from hygiene run
`29397057453` (152s before / 152s after, 0% delta). The job is a one-shot Plan
012 investigation (ephemeral `.config/workspace-hack`, before/after wall-clock
of two full clean workspace builds), not a standing check; re-running it weekly
as a scheduled profile adds no signal. The doc's "exit-checked lane remains as
reproducible evidence" is satisfied by the recorded measurement + commands in
history. Drop the profile; do not enshrine a decided investigation as a named
task. (If reproducibility is ever re-demanded, run the documented commands
manually — not on a schedule.)

### Job 14 — `dylint-advisory` → profile `dylint` (timeout 60, **required**)
- `hygiene-dylint` (mise inline shell): skip-with-success when
  `crates/jackin-lints` is absent (exists on main today, so live); else
  `PATH="$PWD/target/dylint-tools/bin:$PATH" cargo dylint --all -- --workspace
  --ignore-rust-version`, tee `dylint-findings.txt`, propagate exit status.
- **Preserve required-ness**: despite the `-advisory` name the job has no
  `continue-on-error` and `exit "$status"` fails it. Flag the name/semantics
  mismatch; do not silently downgrade to advisory. Tools: `rust cargo-binstall
  cargo:cargo-dylint cargo:dylint-link`. Artifact: `dylint-findings.txt`.

### Job 15 — `dind-chaos` → profile `dind` (timeout 90, required, runner `github`)
- `hygiene-dind-preflight` — `docker info --format …` + `docker buildx version`
  (fails fast without a daemon).
- `hygiene-dind-e2e` — existing `cargo xtask ci --only e2e`.
- Runner stays `github` (fleet-design exception in job comment: needs a Docker
  daemon; OrbStack panics on macOS runners; Velnor has no macOS).
- **Gap:** `JACKIN_CHAOS_SEED` comes from a dispatch input; check_profiles has
  no inputs — profile `env` can only pin a static seed or omit it (default
  seed). Decide at implementation; do not fake input plumbing.
- Tools: `rust zig cargo-binstall aqua:nextest-rs/nextest/cargo-nextest
  cargo:cargo-zigbuild`.

### Job 16 — `health-trend` → profile `health-trend` (timeout 30, advisory) — PARTIAL
- `hygiene-health-snapshot` → extend xtask: `cargo xtask health --snapshot`
  emitting the envelope the job hand-builds with `jq` (schema 1, UTC
  `observed_at` + unix, `commit_sha`, embedded `health --format json` report)
  plus history append. Envelope fields are product-owned; they must not stay
  shell in a `mise run` task.
- **Not extractable:** the "restore prior snapshots" step downloads up to 12
  prior `health-snapshot-*` artifacts via `gh api` + `unzip` + `jq -c` to seed
  `health-history.jsonl`. check_profiles renders checkout → tools → tasks →
  upload; there is no cross-run artifact-download primitive. Either the
  generator gains artifact-history support or the profile starts unseeded each
  run (each snapshot still valid; trend depth rebuilds). Do not smuggle `gh api`
  artifact plumbing into the product task — same reasoning as job 1.
- The `if: github.ref == main` gate has no equivalent; scope the profile's
  schedule accordingly. Artifacts: `health-snapshot.json`, `health-history.jsonl`.

## 5. Shared helper logic (one helper vs per-task duplication)

1. **`mbx` (Mr. Boxington cargo wrapper) — biggest cross-cutter.** Used by jobs
   2 (deny), 3, 4, 5, 6, 13, 16. `mbx` is installed by `mr-boxington-action`,
   is not a mise tool, and check_profiles provisions nothing but mise tools.
   Every `mbx`-prefixed task body fails as-is. Decide once, not per task:
   (a) tasks use plain `cargo` (loses remote-cache acceleration), or
   (b) generator provisions mbx for check_profiles, or (c) an `mbx` mise
   backend is added so profiles can declare it in `tools`. Until decided, no
   `mbx` task is runnable.
2. **CI environment block** (`CARGO_INCREMENTAL=0`, `RUSTC_WRAPPER=sccache`,
   mold `RUSTFLAGS`, `SCCACHE_*`, `MBX_GC_MAX_SIZE`, mold/sccache setup steps)
   repeats across jobs 2–6, 8, 13, 16. Generic, stays generator-side (profile
   `env` or renderer defaults) — never per-task.
3. **`$GITHUB_STEP_SUMMARY` markdown** is written by ~12 jobs. Inside
   `mise run` tasks the var still works, but 12 bespoke echo blocks will rot.
   One helper: extend xtask (there is already a `ci-result` module) with a
   summary-emit helper all hygiene tasks call; task bodies pass data, not markup.
4. **rustup provisioning one-liners** (`component add rust-analyzer/llvm-tools`,
   `toolchain install beta+nmkclippy / nightly+miri`) — keep inline per task
   (one line each, product-owned toolchain needs); no helper warranted.
5. **`gh api` plumbing** (jobs 1, 16-history) — generic CI I/O, excluded from
   product tasks in both cases (§4). If needed later, one generator feature,
   not per-task shell.
6. **Outcome-collection loops** (miri per-crate, hakari before/after — dropped,
   dylint status propagation) — the miri loop is the only survivor; it lives in
   the new xtask subcommand, establishing the pattern for future advisory loops.

## 6. Gaps the extraction depends on (generator-side, not task-side)

- G1 `mbx` provisioning (see §5.1) — blocks jobs 2–6, 16 as specified.
- G2 No cargo-target / registry caching in check_profiles — every profile run
  rebuilds cold (180-min miri, workspace benches, `--release` builds). Needs a
  cache answer before schedules go live.
- G3 No `$CI_XTASK` prebuilt-xtask shortcut — tasks calling `cargo xtask`
  compile xtask from source each run (compounds G2).
- G4 No matrix (→ miri loops in xtask, §4 job 11), no dispatch inputs (→ static
  `JACKIN_CHAOS_SEED`, §4 job 15), no cross-run artifact download (→ unseeded
  health history, §4 job 16), no ref gates (→ schedule scoping for health).
- G5 `ci-fuzz` unrunnable until the `CI_CARGO_FUZZ` hard requirement is fixed
  (§4 job 2 prerequisite).

## 7. Suggested implementation order

1. Fix `ci-fuzz` binary resolution (unblocks all fuzz tasks; removes the
   raw-`cargo fuzz` drift).
2. Add xtask `cold-start` + `hygiene miri` + `health --snapshot` (the three new
   Rust homes).
3. Add `mise.toml [tasks.hygiene-*]` entries (jobs 2–12, 14–16; ~24 tasks).
4. Resolve G1–G2 with the generator owner, then declare the 14 profiles.
5. Drop `cache-usage` and `hakari-timing` from the profile set with the
   evidence in §4 cited in the PR.
