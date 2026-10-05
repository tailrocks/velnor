# Slice C: closure reuse (affected selection, result reuse, required checks)

Status: implemented in the worktree, uncommitted. `cargo test -p velnor-workflow`
(518 lib + 42 integration), `cargo clippy -p velnor-workflow --all-targets`
(0 findings), `cargo fmt --check` all pass. No `GENERATOR_REVISION` bump:
rendered bytes are unchanged (existing byte-stability tests prove it).

## The abstraction

One auditable input/dependency model in `crates/velnor-workflow/src/reuse.rs`,
exercised end to end by four additive CLI verbs:

```text
select  ->  fingerprint  ->  reuse-decision  ->  aggregate
 (which        (artifact       (may a prior      (did the
  units)        identity)        success back      plan complete?)
                                this verdict)
```

- Selection: `select_affected` maps `git diff --name-status -M` entries onto
  `(id, watch, depends_on)` units. Renames match both sides, deletes still
  select their owner, unmatched/global paths fall back to full with a recorded
  reason, and `depends_on` edges expand across unit kinds (dependents run full
  scope, prerequisites join the required set). Every selected unit gets an
  explanation.
- Artifact identity: `canonical_fingerprint` digests source blobs (from
  `git ls-tree -r` filtered by watch + cache key files), the config digest,
  the effective recipe (lane commands + generator revision + check impl), the
  reviewed action pins plus unit tool pins, the transitive dependency
  fingerprints, and the live-state flag. The locator (`unit-<prefix16>`) finds
  candidates; acceptance always compares full digests.
- Reuse: `validate_reuse` reuses only when the evidence proves the same full
  fingerprint, the same recipe, a passing producing aggregate, coverage of the
  complete expected check set, compatible trust (trusted flows down only),
  and — for live-state checks — explicit freshness covering now. There is no
  name field to match: missing evidence reuses nothing.
- Aggregate: `aggregate` scores reports against the planner's `ExpectedWork`.
  Explicit planned no-work passes; missing results, unexpected skips, failed
  prerequisites (even behind a reported success), cancelled required work,
  incomplete matrices, duplicates, and contradictory plans fail. Every item
  gets a stable `executed/reused/skipped/...` explanation, and the verdict
  carries the complete expected check set producing evidence must cover.
- Names: `REQUIRED_CHECK` (`ci-required`) is the single source; per-item
  stable names are pure `ci/<unit>/<lane>[<entry>]`.

## Files touched

- New `src/reuse.rs`: the whole model above plus file schemas (`select` JSON,
  `fingerprint` JSON, evidence/request JSON, expected-work/results JSON) and
  31 unit tests.
- New `tests/closure_reuse.rs`: 11 CLI tests incl. unexpected-skip rejection
  and planned no-work acceptance.
- `src/runtime.rs`: `aggregate`, `select`, `fingerprint`, `reuse-decision`
  arms (via `try_run_reuse`), their commands, `git_name_status`/`git_ls_tree`
  helpers, and `slice_c_selection_agrees_with_runtime_selection`, which pins
  the new core to the planner's selections on a shared fixture. Also removed
  the `CiUnit` dead-code expectation the fingerprint reader fulfilled.
- `src/primitives/ir.rs`: the four `ci-required` render sites now read
  `reuse::REQUIRED_CHECK` (byte-identical).
- `src/lib.rs`: `mod reuse;`, and the ruleset default check reads
  `reuse::REQUIRED_CHECK` (byte-identical).

## Wiring points for the integrator

1. Plan emits expected work. `runtime.rs::plan()` writes the selection file;
   beside it, emit the `ExpectedWorkFile` JSON the `aggregate` command reads:
   selected units × admitted lanes (`plan_lanes`/`selection_for_lanes`),
   `planned_no_work` when the selection is empty, `planned_skip` reasons where
   the planner deliberately skips.
2. Planner selection adopts the core. `runtime.rs::selection_for_diff()` keeps
   its refined path (version-bump fast path, workspace gates, lane filter);
   swap its `git_changed_files` + inline glob core for `git_name_status` +
   `reuse::select_affected` (the cross-check test above is the equivalence
   proof to keep green while merging the refinements down).
3. Unit jobs record evidence. In `run_units_with_selection_file()`, fingerprint
   at start (the `fingerprint_command` core over `ordered_units`), and on
   success write the `EvidenceFile` beside the artifact: fingerprint, recipe,
   executed checks, aggregate pass of the producing run, trust from the event
   class, `fresh_until` for live-state units.
4. Live-state marking. `requires_trusted`/services are generation-time only
   and never reach `project.toml`; `fingerprint --live` takes the list
   explicitly today. Pick the propagation (new unit row vs generation-time
   list) when wiring point 3.
5. Required check. `ir.rs::render_nodes_required()` keeps its shell gate (the
   three-surface admission contract is untouched); optionally shell out to
   `aggregate` for the auditable report. The check name cannot drift: render
   and ruleset default share the constant.
6. Matrices. `ExpectedUnit.matrix` is per-unit entries each lane must report;
   current CI matrices (`write_kind_matrices`) are per-kind unit lists. Map
   one to the other when wiring point 1, so partial matrices reject as
   incomplete instead of passing short.
