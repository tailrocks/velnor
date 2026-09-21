# Change-aware minimal-work CI/CD generation

Date: 2026-09-21. Branch: `integrate/change-aware-minimal-work` (base `89a963f7`).
Goal: Velnor generates the smallest sound execution plan from the full event
change set and discovered/declared task dependencies. Proven irrelevant
changes schedule no unrelated workloads.

## Incident (jackin PR #1016, parent-verified)

- Head `631f8f76`, base `d0d4ee09`, merge-base = base, synthetic merge `2b930129`.
- Whole-PR diff: exactly one file, `M AGENTS.md` (+6/-1).
- Runs at head: exactly 3 (CI/PR `35544119651`, policy `35544119469`,
  renovate `35544119478`). CI/PR ran 45/45 jobs, 0 skipped before runner,
  ~20m17s wall; Planning logged `scope=affected`, `units == full_units ==
  40/40`, 0 pruned. All success, attempt 1.
- Runtime pinned `4fa7a3a85` in all runs.

## Root cause (3 independent researchers + parent verification)

`selection_for_diff` / `select_affected` treat ANY changed file matching
zero unit watch sets as "unknown" and return `full_selection` (every unit):

- s1 live path (jackin is schema=1, routed via `s2/dispatch.rs`):
  `crates/velnor-workflow/src/runtime.rs:2229-2311`, fallback at `2301-2303`.
- Auditable core mirror: `src/reuse.rs:241-323` (`279-287`
  "unmatched path selects every unit") and `src/s2/reuse.rs` equivalent.
- `.github/` blanket full-selection hardcoded in 4 places
  (`reuse.rs:54`, `s2/reuse.rs:54`, `runtime.rs:2251`, `s2/runtime.rs:2347`).
- Jackin `project.toml` (40 units: 36 rust + bun + docker + 2 swift) has NO
  docs unit; only Docs units get markdown watch coverage
  (`primitives/watch/watch.rs:92-96`). Root `AGENTS.md` matched zero
  GlobSets -> all 40 units.

Architectural condition: **unknown/unmatched impact and known-irrelevant
(no watchers, no consumers) collapse into one "select everything" branch.**
Missing: known-irrelevant vs unknown-impact distinction, consumer/ownership
resolution for unmatched paths, fallback-reason channel (`UnitSelection`
carries only units+full_units; plan output has scope/base/head/units/
full_units/plan_digest/excluded, no reason/explanations).

What pinned it in: contract tests asserting the behavior
(`runtime.rs:~5577`, `reuse.rs:1861-1884`, `s2/runtime.rs:~5387`);
zero docs coverage (no `content/docs` hits for the fallback contract).

Sound parts to preserve: dependency-expansion direction
(changed -> dependents; prerequisites pulled into `required` without their
own dependents); `check_prerequisites` fail-closed aggregation;
`closure_reuse.rs` planned-no-work acceptance; 3-dot base...head full-history
diff (fetch-depth 0, no truncation).

## Slices (status; corrected by independent review 2026-09-21)

- S0 failing regression on the live `plan` path (`selection_for_diff`
  s1+s2): unmatched-no-consumer change -> zero workload units
  (currently selects all). [done: 4 tests fail, 1833 pass, committed]
- #978 coordination decision (parent, 2026-09-21): implement the
  classifier against the `unit.watch` seam only; do NOT cherry-pick
  #978's unmerged hunks. Node/bun opaque paths stay `Unknown`->full
  (today's sound behavior) until #978 lands, then narrow automatically;
  declared `reads` covers urgent node gaps. Rebase after #978 merges.
- S2-read-proof FIRST (gates S1): per-repo ownership/read-glob discovery
  (watches + command read globs); no hardcoded extension lists. Blocker
  found by review: velnor docs unit watches `*.md` but lints `**/*.md`
  (read-set superset watch-set) — closed by command read-glob extraction.
  [done: classify Owned|Unknown|Irrelevant in reuse core (mirrored S1+S2),
  declared `reads` table compiling into unit.watch, `.github/`
  classification table replacing the blanket, reason channel; independent
  review INTEGRATE, re-review INTEGRATE]
- S1 core fix across ALL 4 selection impls (or collapse `plan` onto
  `reuse::select_affected` core — preferred, kills divergence class) +
  reason channel; update pinning tests (`runtime.rs:5577`,
  `s2/runtime.rs:5552` — not 5387 — `reuse.rs:1863`, `s2/reuse.rs:1863`,
  parity `runtime.rs:5514`/`s2/runtime.rs:5486`). Quoted paths fail
  closed until S3. [done: all 4 callsites converged on the classifier;
  S0 4/4 green; full suite 2019/2019 green; command-vocabulary
  conformance locks all 19 emitted shapes]
- S3 diff robustness: `-z` NUL parsing + `-M` rename handling atomic
  with matcher; `run_units` merge_group guard symmetry
  (`runtime.rs:2044`, `s2/runtime.rs:2129`); merge_group trigger work
  DEMOTED (deliberately omitted per `lib.rs:16143`). [done: NUL
  plan-path collection, rename-both-sides, trusted-event guard helper;
  independent review INTEGRATE, select-path -z accepted as follow-up]
- S4 live no-work proof only (caller skip + ci-required tolerance
  already work; `ExpectedWork` aggregate is CLI-only/unwired — cut
  as new scope). [pending]
- S5 `crates/velnor-workflow/AGENTS.md` lean rule + `content/docs`
  contract docs. [pending]
- S6 regenerate velnor + jackin consumers; real-CI before/after proof;
  small PRs, merge where authorized. Order after #980 (generated-YAML
  churn); S2 coordinated with #978 (WatchGraph overlap). [pending]

Review corrections: `select_affected` (s1/s2 `reuse.rs`) drives only the
`select` CLI, never `plan` — the 4-impl divergence (plan: `--name-only`
no-`-M` + inline match + version-bump + workspace gates; select:
`--name-status -M` + closure + explanations) is the deeper condition.
Keep sound: stale/empty-base->full, git-failure->full, `.github/`
blanket until S2 audit, `version_bump_matches` before irrelevant-skip.

Adjacent open PRs (avoid conflicts): PR #990 (generator-owned
AGENTS.md), #979/#980 (validation contract), #985, #978.

## Decisions

- Fix generic engine first; consumer config changes only for genuinely
  non-discoverable contracts. No hardcoded AGENTS.md/*.md exclusions, no
  hand-edited generated YAML (per `crates/velnor-workflow/AGENTS.md`).
- No legacy/shim paths (per root `AGENTS.md`): replace fallback semantics,
  update pinning tests in the same slice.
- Unknown impact keeps a conservative fallback, but smallest sound scope +
  explicit reason + missing-contract report — never silent full selection.
- S2 design accepted (read-only designer, 2026-09-21): per-repo
  `classify(path) -> Owned{units,reasons} | Unknown{consulted}` in the
  reuse core (mirrored S1+S2); sources = declared contracts > watch globs
  > sound command read-glob extractors (no extension inference, no shell
  parsing); `Unknown` keeps fail-closed full selection. Declared
  `reads` table is generation-time only, compiles into `unit.watch`
  (no runtime schema-3 bump). `.github/`: 4 global classes stay full,
  kind reusables narrow by kind, release/maintenance narrow by PR scope.
  `REUSE_VERSION` bumps. Rebase onto #978 node-watch hunks first; seam
  is the final `unit.watch` vec. Full design in session record
  (S2 read-proof ownership design).
