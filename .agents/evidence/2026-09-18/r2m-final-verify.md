# R2m final verification: tailrocks/velnor#930 @ bd9952e1 (post needle-fix update)

## VERDICT: HOLD — do not merge

The needle fix works (units EXECUTE now: 17/17 github-hosted callers + 4/4
non-rust velnor callers ran, zero needle-skips), but CI exposes a second
flip-adopted bug of the same JSON-vs-CSV class: s2 `plan` emits
`needs.plan.outputs.units` as JSON while the reusable writes it raw into the
selection file's CSV-only `units=` field, so `velnor-workflow run` fails
closed in EVERY executed leg (`invalid unit id in CI selection artifact`).
All 17 github-hosted legs fail deterministically on it; `ci-required` and
Policy fail downstream. Merge would turn `main` red the same way.

- Heads verified: `origin/feat/r2m-flip` = `bd9952e1dcf84a4ef0173c50a25a1807948ba1f0`
  (5 commits over `origin/main`); `origin/main` = `52739e176c1cba3ddeeff1ca21eda2edadb52b17`
  (re-fetched at report time; #931 merge).
- Worktree: `/tmp/r2m-final` (bd9952e1, detached).
- Read-only except `/tmp`; nothing pushed, merged, or amended.

## 1. UPDATE INTEGRITY — PASS

- Ancestry: `merge-base --is-ancestor 52739e17 bd9952e1` → YES. Branch log
  over main = exactly the 5 flip commits (24d1df91, 98868635, 11b5c1cb,
  50c30b22, bd9952e1). Fresh: merge-base(feat, main) = origin/main.
- `gh pr view 930`: `mergeable: MERGEABLE` (mergeState BLOCKED = red checks,
  not conflicts), head = bd9952e1.
- Pin: `.github-gen/velnor-workflow.toml:9` =
  `revision = "52739e176c1cba3ddeeff1ca21eda2edadb52b17"`, byte-identical to
  `git rev-parse origin/main`. Operationally live: the CI/PR run published
  `velnor-workflow-runtime-52739e17…-Linux-X64` and all 17 github legs passed
  runtime download + digest verification against it.
- Merge 50c30b22 conflict-free, no hand edits: parents are exactly
  11b5c1cb + 52739e17; `diff(11b5c1cb→50c30b22)` vs `diff(d8138ef0→52739e17)`
  and `diff(52739e17→50c30b22)` vs `diff(d8138ef0→11b5c1cb)` are
  content-identical (only hunk-header line numbers/blob hashes differ).
- Both helpers survive in `s2/primitives/ir.rs`:
  `docker_build_token_env_for_members` (line 1839, used line 4785) and
  `selected_unit_needle` (line 2821, used by both caller line 2830 and
  callee line 3355 emitters).
- DCO: `Signed-off-by: Alexey Zhokhov <alexey@zhokhov.com>` on all 4
  non-merge commits; PR `DCO` check `pass`. (Merge commit carries no
  trailer, as normal.)

## 2. NEEDLE FIX IN RENDERS — PASS (od-level)

- Caller needles in `ci-pr.yml` + `ci-main.yml` are now bare-quote:
  `contains(needs.plan.outputs.units, '"unit_id":"docker"')`.
- Zero backslashes: all `needs.plan.outputs.units` lines with `\` went
  35→0 per file; hexdump of a caller needle shows `27 22 75 6e 69 74…`
  (`'"unit…`) with no `5c` byte anywhere in the expression.
- Byte-identical to callee form: caller `'"unit_id":"docker"'` vs callee
  template `format('"unit_id":"{0}"', inputs.unit)` (ci-unit-docker.yml) —
  identical spelling modulo the `{0}`→id substitution; od of both shows
  bare quotes, no escapes.
- Fixed occurrences: 46 caller `contains(needs.plan.outputs.units, …)`
  needles per file × 2 files = **92 fixed**, 0 escaped needles remain
  anywhere in the rendered tree.
- Pure respelling: after stripping `\`, every old/new needle line pairs
  exactly (no unpaired lines).

## 3. REGEN — PASS

- Fresh worktree @bd9952e1: `cargo build -p velnor-workflow` ok;
  `./target/debug/velnor-workflow --plain --force` → exit 0,
  "Generated 21 files", `git status` clean; `--plain --dry-run` →
  "Dry-run: 0 files would change", exit 0.
- Regen diff 11b5c1cb→bd9952e1 (15 files) fully classified:
  - pin repoint d8138ef0→52739e17: toml pin, ci-policy (rev/BASE_PIN/
    POLICY_REVISION), all 5 ci-unit reusables (artifact name +
    EXPECTED_REVISION ×2, + PINNED_REVISION in rust), maintenance,
    preview, release (21× rev + POLICY_REVISION + PINNED_REVISION),
    generator-state digests, plus the non-needle lines of ci-pr/ci-main
    (rev/POLICY_REVISION/EXPECTED_REVISION/artifact/BASE_PIN — all pin);
  - needle fix: all 92 caller-needle lines in ci-pr/ci-main (pure
    respellings, proven in §2);
  - #931 source: `s2/mod.rs` + `s2/primitives/ir.rs` (shared
    `selected_unit_needle` + regression test).
  - Release has 0 needle lines (event/ref selection, unaffected) —
    all-pin as expected.
- No new weakenings: 0 added `if: false` / `&& false`;
  `== 'skipped'` guards 13=13 in ci-pr.

## 4. GATES — PASS (all rerun in /tmp/r2m-final, all green)

- `cargo test -p velnor-workflow`: **1706 passed, 0 failed** (lib 1589 =
  prior 1588 + the new `caller_and_callee_selectors_share_one_plan_json_needle`
  test; 117 integration).
- `cargo test -p velnor-runner`: **2209 passed, 0 failed, 4 ignored**.
  Matches prior.
- Contract suite (`crates/velnor-workflow-contract`): **6/6** (2 + 4).
- `cargo clippy --all-targets -p velnor-workflow -- -D warnings`: clean.
- `cargo fmt --check`: clean. `actionlint -config-file .github/actionlint.yaml`: clean.

## 5. CI — FAIL (the crux; terminal, nothing in flight)

Runs at head bd9952e1 (both `completed`/`failure`):
CI/PR `35220938667`, Policy `35220935753`. 74 jobs: **1 success** (Planning),
**24 failure, 49 skipped**.

Executed vs skipped unit/provider jobs:
- RAN (25): Planning + 17/17 github-hosted unit legs + 4/4 non-rust velnor
  legs + prepare-cargo + ci-required + Control/Required.
- The needle fix is proven by execution: every caller whose `if:` could be
  true ran. Zero skipped-by-needle.
- Skipped 49 = 36 structural cross-provider legs (17 velnor legs inside
  github-hosted reusables + 4 github legs inside velnor reusables + 13 rust
  prepare-cargo legs + 2 Prepare-Cargo legs) + **13 rust velnor-side callers**.

Why the 13 rust velnor-side callers skip (NOT the needle): all 13 carry
`needs: [plan, prepare-cargo, …]` + the conjunct
`(needs.prepare-cargo.result == 'success' || … == 'skipped')`, and
prepare-cargo FAILED at velnor admission (`Velnor rejected this job before
workflow execution… operational store rejected the sanitized admission row;
job failed closed before execution`). Non-rust velnor callers (`needs:
[plan]` only) all ran — the single differing conjunct fully explains the
13 skips as an environmental cascade. (Still: 13 unit jobs did not run.)

Why all 17 github-hosted legs FAIL (flip bug #2, blocking): every one fails
in the checks step with byte-identical error (17/17 verified from logs):
`error: invalid unit id in CI selection artifact: [{"providers":["github-hosted"…`
from `velnor-workflow run --config .github/ci/project.toml --scope affected …`.
Mechanism, all links proven:
 1. s2 `plan` emits output `units` as JSON (`units_json`, s2/runtime.rs
    `writeln!(file, "(\"units\", units_json…)"`) — live value from the run:
    `[{"providers":["github-hosted","velnor"],"unit_id":"bun-velnor"},…]`
    (ci-required NEEDS_JSON). `full_units` stays CSV.
 2. The reusable's "Materialize Velnor CI selection" writes
    `units=${{ inputs.selected_units }}` = `needs.plan.outputs.units` =
    that JSON (s2/primitives/ir.rs:4292,4918 — inherited s1 wiring).
 3. `run`'s `parse_selection_ids` (s2/runtime.rs:2271, same in s1) splits
    on commas and demands `is_unit_id` per element → rejects `[…` fail-closed.
In s1 the same wiring worked because plan `units` was CSV
(`contains(format(',{0},', inputs.selected_units)…)` + `units=a,b,c` test).
s2 changed the planner output format without changing the consumer — the
same JSON-vs-CSV boundary class as the needle bug, one layer deeper. No
test covers plan-output → selection-file → `run` end to end (string tests
can't catch it; second instance of the structural gap).

Velnor-side distinction (ran-and-failed-environmentally, as expected for the
4 that ran): all 4 non-rust velnor legs show conclusion `failure` (not
`skipped`) with log group `Velnor rejected job (operational_store)` and
`failed closed before execution` — admission attempted pre-bastion, vs a
needle skip which would be conclusion `skipped` with no steps.

- `ci-required`: FAILS fail-closed and correctly:
  `expected CI job github-hosted-bun-velnor did not pass: failure`
  (verdict binds plan digest 8563e4d2f58b08d6). Violations are failures,
  not skips — but NOT environmental-admission, so not excusable.
- Policy: FAILS via the candidate path with candidate
  `velnor-workflow-candidate-2d15899eaaa12417-Linux-X64`:
  `no same-repository PR run published candidate …`. Root-caused: candidate
  packaging is a ci-unit-rust step after checks; checks die on the selection
  bug, so the run publishes exactly 1 artifact (pinned runtime from
  Planning), 0 candidates.
- No wait was needed: CI is terminally red, not pending.

Suggested fix (not authored): `plan` must emit the affected set as CSV
alongside the JSON (or the emitter must stop feeding JSON into the CSV-only
`units=` field); add an end-to-end regression test running `run` against a
selection file materialized from live plan outputs. The pinned generator at
52739e17 carries this bug too (planner + run both), so the follow-up fix
needs a new pin-bump, like #931 did.

## 6. POST-MERGE PREDICTION + ROLLBACK

- If merged as-is, `main` goes red on the next `ci-main`: all 21 executed
  legs fail at checks with the selection error (github) or admission
  rejection (velnor), `ci-required` fails fail-closed. Policy-on-main
  pin-regen determinism is irrelevant against red required checks.
- After a correct fix + regen + green CI, steady state: 17 github-hosted
  legs SUCCESS; velnor legs ran-and-failed-environmentally until bastion
  admission accepts the flipped tree (pre-existing environmental condition,
  out of this PR's scope); Policy green via candidate path; ci-required
  green modulo the velnor-admission question for required checks.
- Rollback path: single `git revert` of the merge restores s1 config/tree
  and the old pin cleanly — all flip state is inside the PR.

## Per-item scorecard

1. UPDATE INTEGRITY — PASS. 2. NEEDLE FIX IN RENDERS — PASS (92/92,
od-proven). 3. REGEN — PASS (clean + dry-run 0, every hunk classified).
4. GATES — PASS (1706 + 2209 + 6, clippy/fmt/actionlint clean).
5. CI — FAIL (blocking: flip bug #2 kills all checks; 13 velnor-rust
callers skip via environmental cascade). 6. predicted main red; rollback clean.

**VERDICT: HOLD** — the needle fix itself is verified working (units
execute), but merging would adopt a second flip bug that fails every unit
leg. Do not merge until the plan-JSON → selection-file format mismatch is
fixed, the tree regenerated + repinned, and CI is green with github-hosted
legs passing.
