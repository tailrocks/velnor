# Plan 046: `velnor-tools audit-ci` + `compare` productization

> **Executor instructions**: Follow step by step; run every verification; STOP
> conditions binding. Update the status row in `plans/README.md` when done.
>
> **Drift check (run first)**:
> `git diff --stat 48b04ad..HEAD -- crates/velnor-tools/`
> Re-locate by symbol; mismatch → STOP.

## Status

- **Priority**: P1 (V1.16 + §2.8 enforcement; hard prerequisite of plan 058)
- **Effort**: M/L
- **Risk**: LOW (read-only tooling)
- **Depends on**: none (rule set from `VELNOR_PROJECTS_SETUP.md` §2; perf
  mode consumes plan 045's summaries when present but parses raw logs
  regardless)
- **Category**: dx / tests
- **Planned at**: commit `48b04ad`, 2026-07-18

## Why this matters

Thirteen repos will drift back without an enforcement tool. §2.8 of
`VELNOR_PROJECTS_SETUP.md` (READ §2.0–§2.11 FIRST — it is the rule source)
specifies `audit-ci`: fail on `ubuntu-latest`, missing lanes, dtolnay,
missing sccache on compile jobs, missing `concurrency`/`timeout-minutes`,
uncommented `fetch-depth: 0`, double-cache stacks, lane-conditional steps
beyond the two sanctioned forms, action majors behind latest upstream,
deprecated workflow commands — plus a perf mode failing warm runs that show
`Downloading`/`Compiling`/tool-install markers. The dual-lane `compare` also
needs productizing into the V-B gate tool (lane-compare exists and is close).

## Current state (verified by the tools audit; re-locate by symbol)

- `crates/velnor-tools/src/main.rs` — `CommandKind` enum `main.rs:31-67`;
  dispatch `main.rs:484-507`; registration pattern = 3 edits (variant, match
  arm, args struct; `LaneCompare` at `:66`/`:505` imports from its own
  module `lane_compare.rs` — new subcommands follow that shape:
  `mod audit_ci;` + `audit_ci.rs`).
- Reusable YAML walkers (serde_yaml = `noyalib` fork, `Cargo.toml:29`):
  `read_target_workflows` `main.rs:1910`, `collect_workflow` `:1984`,
  `collect_step` `:2051` (already flags `fetch-depth` at `:2076`),
  `normalize_uses` `:2091`, `mapping_get`/`object_get` `:2144-2152`.
  The rule-style template: `check_matrix_parity_job` `main.rs:4198-4282`
  ("parse YAML → Vec<String> issues → bail if non-empty").
  NOTE: `target_audit` (`main.rs:1828-2692`) is drift-by-expected-count —
  reuse its walkers, NOT its assertion model.
- Lane-compare: `lane_compare.rs` (1432 lines) — one-run diff
  (`lane_compare` `:116-256`, LCS step alignment `:831`, verdicts `:970`),
  watch/regression mode LANDED (plan 030: `lane_compare_watch` `:258-319`,
  `is_regression` `:632-667`, reports under `.velnor-compare/`). 14 tests
  `:1133-1432`.
- GitHub access: `gh` CLI subprocess (`gh_api_bytes` `lane_compare.rs:410`);
  timing derived client-side from step `started_at/completed_at`
  (`:805`, `:941`).
- Perf markers: greenfield — the contract lines are documented
  (`docs/perf-instant-cache-plan-2026-06-11.md:152` "second run shows zero
  `Downloading`/`Updating` lines, zero dependency `Compiling` lines").
- velnor-tools is dev-invoked (`cargo run -p velnor-tools -- <cmd>`), not
  packaged — fine for this plan.

## Commands you will need

| Purpose | Command | Expected |
|---------|---------|----------|
| Gates | fmt / clippy / `cargo nextest run --workspace --locked` | exit 0 |
| Focused | `cargo nextest run -p velnor-tools audit_ci` | new tests pass |
| Smoke | `cargo run -p velnor-tools -- audit-ci --repo-path /Users/donbeave/Projects/tailrocks/holla-project/holla` | findings printed |

## Scope

**In scope**: new `crates/velnor-tools/src/audit_ci.rs` + registration;
small extensions to `lane_compare.rs` (a `compare` alias/subsumption — see
step 4); shared walker extraction ONLY if reuse forces it (prefer calling
the existing `main.rs` fns; move them to a `workflow_yaml.rs` module only if
visibility requires — keep the churn minimal).
**Out of scope**: fixing the estate repos (estate plans); packaging
velnor-tools; the fixture registry (plan 041); CI wiring of audit-ci into
the 13 repos (plan 058).

## Git workflow

Branch `velnor-estate-standard`; `feat(tools):` commits, `git commit -s`; no push
without operator instruction.

## Steps

### Step 1: `audit-ci` static rules

`audit_ci.rs`: `AuditCiArgs { repo_path: PathBuf, #[arg(long)] json: bool,
#[arg(long)] perf_log: Option<PathBuf>, ... }`. Parse every workflow via the
existing walkers. Rules (each = code + one-line finding with file + JSON
path + remedy; severity ERROR unless noted):

1. `runs-on` contains `ubuntu-latest`/`ubuntu-24.04`/any `macos-*`/`windows-*`.
2. Workload workflow (has push/pull_request triggers) lacks the `lanes`
   dispatch input or the inline matrix marker (`inputs.lanes == 'both'`
   substring in a job's `strategy.matrix.config`).
3. Any `dtolnay/rust-toolchain` / `EmbarkStudios/cargo-deny-action` use.
4. Compile job (steps run `cargo `/`rustc`) without a sccache setup action —
   and WITH one, env `SCCACHE_GHA_ENABLED` ≠ "false".
5. Missing workflow-level `concurrency`; any job missing `timeout-minutes`.
6. `fetch-depth: 0` without a same-step `#` comment (walker already reads
   checkout inputs at `collect_step` `:2076`; comments need the raw-line
   scan — read the file text for the step's line span; if comment
   attribution is too brittle, downgrade to WARN and note).
7. Double-cache: job has ≥2 of {Swatinem/rust-cache, sccache setup action,
   `actions/cache` with a `target`/`~/.cargo` path in the SAME concern} —
   flag Swatinem+sccache always, and `actions/cache` on `path: target`.
8. Lane-conditional steps: any step-level `if:` mentioning
   `matrix.config.lane` (allowed: none) or `matrix.config.writer`
   (allowed — the sanctioned form); any hardcoded runner-name strings in
   run bodies (reuse `find_hardcoded_lane_strings` `main.rs:4287`).
9. Floating `uses:` refs (no 40-char SHA) → ERROR; SHA behind latest
   upstream major → WARN (resolve latest via
   `gh api repos/<o>/<r>/releases/latest`, cache per invocation, `--offline`
   flag skips this rule).
10. Deprecated commands in run bodies: `::set-output`, `::save-state`,
    `actions/create-release`, node12/16 markers.
11. Uniform-shape (§2.12): workload workflow filenames outside the canonical
    set for their concern (`ci.yml`, `release.yml`, `docs.yml`,
    `preview.yml`, `renovate.yml` — heuristic: a workflow with
    push+pull_request triggers not named `ci.yml` → WARN, ERROR when a
    canonical concern uses a non-canonical name); job ids outside the
    canonical vocabulary (`rust`, `integration`, `audit`, `build-image`,
    `docs`, `release`, `ci-required`) for jobs matching those concerns →
    WARN; concurrency group not `<workflow-name>-${{ github.ref }}` → WARN;
    missing `.github/AGENTS.md` → ERROR.

Output: human table + `--json`; exit 1 on any ERROR.

**Verify**: unit tests per rule on inline YAML fixtures (≥ 10 tests; model
the `check_matrix_parity_job` tests near `main.rs:4340+`). Smoke against
holla (expect: timeout findings pre-051) and against the migrated fixture
(expect: clean, post-041).

### Step 2: Perf mode

`audit-ci --perf-log <file|run-id>`: given a warm-run job log (file path, or
fetch via `gh` like `lane_compare.rs:424`), fail on: `Downloading crates`/
`Updating crates.io index`/dependency `Compiling <crate> v` lines (allowlist:
the workspace's own crate names — take from `--first-party <names>` or parse
the repo's Cargo.toml package names when `--repo-path` given), tool-install
markers (`installing`, mise download lines during setup steps). Reuse
`analyze_lane_log`-style line scanning (`lane_compare.rs:888`).

**Verify**: unit tests with synthetic logs (cold log fails, warm log passes,
first-party compile allowed).

### Step 3: Estate sweep mode

`audit-ci --estate <file>` — a TOML/JSON list of repo paths (default file
checked into the velnor repo listing the 13 clones from
`VELNOR_PROJECTS_SETUP.md` §13) → one table, exit 1 if any repo errors.
This is plan 058's engine.

**Verify**: sweep over 2 tempdir fake repos in a test.

### Step 4: `compare` = lane-compare, promoted

Add `Compare` as an alias subcommand of `LaneCompare` (same args) OR rename
with a deprecation alias — pick alias-only (zero churn). Extend the report
header with the §2.11 budget verdicts for the Velnor lane (pickup, wall vs
class budget — class passed via `--class D|B|A|C`; budgets as constants
mirroring `VELNOR_PROJECTS_SETUP.md` §2.11 with a doc-pointer comment).

**Verify**: `cargo run -p velnor-tools -- compare --help` works; budget
verdict unit test on synthetic stats.

## Test plan

≥ 15 new tests (rules + perf + sweep + budgets); full gates. Real-world
smoke on two estate clones recorded in the PR.

## Done criteria

- [ ] Gates exit 0; tests pass; smoke outputs match repo reality
- [ ] All ten §2.8 rule families implemented (or explicitly WARN-downgraded
      with reason in code comment)
- [ ] Perf mode catches the documented marker set; estate sweep works
- [ ] `compare` alias + budget verdicts live
- [ ] No out-of-scope changes

## STOP conditions

- The noyalib YAML fork cannot expose comments/line numbers needed for rule
  6 → downgrade to WARN via raw-text scan; if even that misattributes, STOP
  for that rule only, note it, ship the rest.
- Latest-release resolution rate-limits (`gh` unauthenticated) → require
  `GITHUB_TOKEN` for rule 9, degrade to floating-ref-only checks offline.

## Maintenance notes

- Rules mirror `VELNOR_PROJECTS_SETUP.md` §2 — every future standard change
  updates BOTH (reviewers enforce; the doc is the spec, the tool is the
  teeth).
- Plan 058 wires the estate sweep + weekly schedule; keep `--json` stable
  for it.
