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

## Slices (status)

- S0 failing regression: unmatched-no-consumer change -> zero workload
  units (currently selects all). [pending]
- S1 core selection fix + reason channel; update pinning tests. [pending]
- S2 generic consumer/ownership discovery, `.github/` audit, typed declared
  contracts for non-discoverable relations. [pending]
- S3 diff robustness: NUL-delimited parsing, rename parity, merge_group
  semantics, event-semantics tests. [pending]
- S4 no-work gate/cache verification end to end. [pending]
- S5 `crates/velnor-workflow/AGENTS.md` lean rule + `content/docs`
  contract docs. [pending]
- S6 regenerate velnor + jackin consumers; real-CI before/after proof;
  small PRs, merge where authorized. [pending]

Adjacent open PRs (avoid conflicts): #990 (generator-owned AGENTS.md),
#979/#980 (validation contract), #985, #978.

## Decisions

- Fix generic engine first; consumer config changes only for genuinely
  non-discoverable contracts. No hardcoded AGENTS.md/*.md exclusions, no
  hand-edited generated YAML (per `crates/velnor-workflow/AGENTS.md`).
- No legacy/shim paths (per root `AGENTS.md`): replace fallback semantics,
  update pinning tests in the same slice.
- Unknown impact keeps a conservative fallback, but smallest sound scope +
  explicit reason + missing-contract report — never silent full selection.
