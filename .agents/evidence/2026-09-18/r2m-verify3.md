# R2m round-3 verification: tailrocks/velnor#930 @ 17d4867b (post selection-fix update)

## VERDICT: HOLD — do not merge

The selection fix works (17/17 github-hosted legs now PASS; every local
gate green), but CI exposes a third flip-adopted incompatibility, this one
structural: the flip renamed the dogfood config field
`requires_trusted = true` → `trust = "trusted-only"` (commit 98868635),
and the Stage-0 Policy validator that `pull_request_target` executes —
base main's product at a6fa8d4a, which predates the s2 schema — hard-errors
parsing the PR tree (`unknown field 'trust'`) before the candidate
exception can engage. Policy red is therefore PR-caused, not the
velnor-admission baseline. Merge would also land main in an all-legs-skip
red state (proven by replay: the post-merge pin check exits 1 with 11
differing files, and ci-main gates every leg on it).

- Heads verified: `origin/feat/r2m-flip` =
  `17d4867b39d8ed73306ed15a8bb0a273f9ed8e43`;
  `origin/main` = `40206d9fe60a8e0693046d1695762c62883a0578`
  (re-fetched at report time; #932 merge).
- Worktrees: `/tmp/r2m-v3` (17d4867b, detached; regen + gates),
  `/tmp/r2m-402` (40206d9f, detached; pin-binary replay).
- Read-only except `/tmp`; nothing pushed, merged, or amended.

## 1. UPDATE INTEGRITY — PASS

- Ancestry: `merge-base --is-ancestor 40206d9f 17d4867b` → YES.
  Branch over main = flip commits + merge a0a15d29 + pin/regen 17d4867b.
- `gh pr view 930`: `mergeable: MERGEABLE` (mergeState BLOCKED = red
  checks, not conflicts), head = 17d4867b.
- Pin: `.github-gen/velnor-workflow.toml:9` =
  `revision = "40206d9fe60a8e0693046d1695762c62883a0578"`,
  byte-identical to `git rev-parse origin/main`.
- Product `velnor-workflow-runtime-v1-64fdd0bf7a8a5800` linked to 40206d9f
  three independent ways (not timing):
  1. release `targetCommitish = 40206d9f…` (full SHA, exact);
  2. locally recomputed closure over 40206d9f's `ls-tree`
     (`LC_ALL=C sort` + `closure-version:1/features:/profile:release`
     footer, per `s2/closure.rs`) =
     `64fdd0bf7a8a5800e014bb303e983c5fbf5c460b27cbaa9eff550482d7ef22a0`
     — matches the tag prefix AND `manifest.json` `.closure` byte for byte;
     manifest `.revision = 40206d9f…`;
  3. publisher run
     https://github.com/tailrocks/velnor/actions/runs/35225970946
     (`ci-runtime-products.yml`, headSha 40206d9f, success).
- Merge a0a15d29: parents exactly bd9952e1 + 40206d9f. One file
  conflicted (`.github/ci/.github-actions-generator-state`, changed on
  both sides); resolution = `--theirs` (byte-identical to main's copy).
  Full replay in a scratch worktree (`git merge --no-commit` + theirs
  resolution + `git write-tree`) reproduces the recorded tree
  `432b2f64…` exactly → zero hand edits to code; `s2/mod.rs`,
  `s2/primitives/ir.rs` auto-merged, `s2/runtime.rs` +
  `tests/selection_plan_handoff.rs` taken from #932.
- DCO: `Signed-off-by: Alexey Zhokhov <alexey@zhokhov.com>` on 17d4867b
  (and on a0a15d29 + all non-merge commits); PR `DCO` check `pass`.

## 2. SELECTION FIX IN RENDERS — PASS

End-to-end CSV channel proven at HEAD, source through renders:

- Source (#932, verbatim: `crates/` delta bd9952e1→17d4867b is
  content-identical to 52739e17→40206d9f, only hunk offsets differ):
  `s2/runtime.rs:1621` builds `unit_ids` as
  `planned.iter().map(unit_id).join(",")` (CSV) and emits a new
  `unit_ids` plan output; `s2/primitives/ir.rs:4292,4918` repoint the
  selection-file `units:` binding from `inputs.selected_units` (JSON) to
  `inputs.selected_unit_ids` (CSV).
- Renders: all 11 `SELECTION_UNITS:` lines (rust 3, bun/docker/docs/
  opentofu 2 each) now read `${{ inputs.selected_unit_ids }}`; zero
  `SELECTION_UNITS: ${{ inputs.selected_units }}` remain anywhere.
  Callers pass `selected_unit_ids: ${{ needs.plan.outputs.unit_ids }}`
  (70 lines: 35 each in ci-pr/ci-main); plan exposes
  `unit_ids: ${{ steps.plan.outputs.unit_ids }}` (ci-pr.yml:46,
  ci-main equivalent); all 5 reusables declare the new required input.
- Regen diff bd9952e1→17d4867b (17 files) fully classified, all 299
  rendered +/- lines:
  - pin repoint 52739e17→40206d9f: 164 lines, 82 minus / 82 plus,
    zero cross-direction (toml pin, rev/POLICY_REVISION/
    EXPECTED_REVISION/artifact names; release/maintenance/preview/
    ci-policy hunks are ALL pin);
  - selection fix: 88 `unit_ids` plus-lines (70 caller passthrough +
    11 SELECTION_UNITS + 2 plan outputs + 5 input declarations) +
    11 removed JSON-fed SELECTION_UNITS + 10 input-block
    `required:/type:` lines;
  - generator-state digests: 26 lines (regen consequence);
  - #932 sources: `s2/mod.rs`, `s2/primitives/ir.rs`, `s2/runtime.rs`,
    `tests/selection_plan_handoff.rs` (2 new tests).
  164 + 88 + 11 + 10 + 26 = 299. Zero unclassified.
- No needle drift: `contains(` diff hunks = 0; needles 35/35 per file,
  0 escaped, byte-counts identical bd vs HEAD.
- No weakenings: 0 added `if: false` / `&& false`;
  `== 'skipped'` guards 13=13 in ci-pr.

## 3. REGEN — PASS

Fresh worktree @17d4867b: `cargo build -p velnor-workflow` ok;
`--plain --force` → exit 0, "Generated 21 files", `git status` clean;
`--plain --dry-run` → "Dry-run: 0 files would change", exit 0.

## 4. GATES — PASS (all rerun in /tmp/r2m-v3, all green)

- `cargo test -p velnor-workflow`: **1709 passed, 0 failed** (prior 1706
  + 3 from #932: lib 1590 incl. handoff unit coverage, +2 integration
  `live_plan_outputs_materialize_a_selection_file_run_executes` and
  `plan_json_in_the_units_field_still_fails_closed`).
- `cargo test -p velnor-runner`: **2209 passed, 0 failed, 4 ignored**.
- Contract suite (`crates/velnor-workflow-contract`): **6/6** (2 + 4).
- `cargo clippy --all-targets -p velnor-workflow -- -D warnings`: clean.
- `cargo fmt --check`: clean. `actionlint`: clean.

## 5. CI — FAIL (Policy is PR-caused; everything else green/excusable)

Runs at head 17d4867b, both terminal (`completed`/`failure`, no wait
needed): CI/PR
https://github.com/tailrocks/velnor/actions/runs/35227170209
(74 jobs: 18 success, 7 failure, 49 skipped) and Policy
https://github.com/tailrocks/velnor/actions/runs/35227169020
(1 job: failure). `gh pr checks`: 19 pass / 8 fail / 49 skipping.

(a) 17/17 github-hosted SUCCESS (run URL above): all four non-rust legs
(bun-velnor 27s, docker 3m58s, docs 14s, opentofu 14s) + all 13 rust
legs incl. velnor-workflow 9m53s and velnor-runner 8m43s. The selection
fix is proven by execution: every checks step that died with `invalid
unit id in CI selection artifact` in round 2 now passes.

(b) Velnor-side + prepare-cargo ran-and-failed-environmentally (NOT
skips): all 5 carry conclusion `failure` with the admission group
`Velnor rejected job (operational_store)` /
`operational store rejected the sanitized admission row; job failed
closed before execution` (jobs 105221724528/697/746/338 + prepare-cargo
105221725098). Same pre-bastion condition as every prior round.

(c) POLICY red — PR-caused (blocking). Exact chain, every link proven:
- `pull_request_target` executes BASE main's ci-policy.yml (rev/BASE_PIN/
  POLICY_REVISION = a6fa8d4a, an ancestor of 40206d9f) against the
  audited PR tree. So the running validator predates the s2 schema.
- Candidate path TAKEN and fully bound: PR run published exactly
  `velnor-workflow-candidate-ae2681738c338f05-Linux-X64` (40 MB) +
  the pinned runtime artifact; the name matches local
  `closure --rev=17d4867b --candidate` (`ae2681738c338f05…`) byte for
  byte. Acquire's digest check, manifest-closure check
  (`manifest == head_candidate`), and binary self-report check all
  passed — control flow proves it (any failure `exit 1`s in acquire;
  the run proceeded to Enforce with `VELNOR_WORKFLOW_PINNED_BINARY`
  + `VELNOR_WORKFLOW_CANDIDATE_MANIFEST` set).
- Enforce then runs `velnor-workflow policy … --candidate-manifest …`
  = the BASE a6fa8d4a binary, whose `evaluate()` calls
  `DeclaredTree::read()` (`policy.rs:332` at a6) with `?` — a config
  parse failure is a HARD ERROR before any candidate logic.
- The PR tree's `.github-gen/velnor-workflow.toml:264` says
  `trust = "trusted-only"` (s2 schema, introduced by THIS PR in
  98868635 "flip dogfood generation to schema 2"; main@40206d9f has
  `requires_trusted = true`). The a6fa8d4a s1 parser rejects it:
  `error: invalid generation config … TOML parse error at line 264 …
  unknown field 'trust', expected one of … 'requires_trusted', …`
  (job https://github.com/tailrocks/velnor/actions/runs/35227169020/job/105221596812).
- The expected-field list (`requires_trusted`, no `trust`) proves the
  failing parser is the base binary's s1 struct, not the candidate.
- Baseline comparison — NOT the baseline condition: PR #932's Policy
  run https://github.com/tailrocks/velnor/actions/runs/35224945016
  (generator-change PR, s1 tree) took the candidate path and is
  `completed/success`. The candidate design works when the base
  validator can parse the tree's config; this PR is the first to ship
  a config schema the base validator cannot read. Nothing about
  velnor admission is involved (Policy runs on ubuntu-24.04).

(d) ci-required + Control/Required — environmental-baseline (excusable):
- ci-required fails fail-closed and correctly:
  `verdict binds plan digest 8563e4d2f58b08d6` (same affected set as
  round 2), `expected CI job velnor-bun-velnor did not pass: failure`.
  The trigger is the velnor legs' admission `failure`, not a skip and
  not a PR defect.
- Control/Required is a pure mirror (`if: needs.ci-required.result !=
  'success'` → `run: exit 1`, ci-pr.yml:2240-2248); its 2 KB log is
  literally `exit 1`. Pure downstream cascade.
- Baseline: #932 merged with the IDENTICAL red set (4 velnor fails +
  prepare-cargo fail + ci-required + Control/Required fail; Policy +
  17 github + DCO green). The delta of this PR vs #932's baseline is
  exactly one check: Policy green→red, for the PR-caused reason above.
- Job-for-job vs main@40206d9f CI/main run
  (https://github.com/tailrocks/velnor/actions/runs/35225971311):
  40 jobs, 1 success (Planning) / 3 failure (Policy, ci-required,
  Control/Required) / 36 skipped. Main is red at baseline, but by a
  DIFFERENT mechanism: its pin (a6fa8d4a — #931/#932 never bumped it)
  is stale, so the in-CI policy job's `--check` reports `generated
  files differ` (6 files) and its candidate wait
  (`…-candidate-5063ab07089eee99-…`, verified locally as
  `closure --rev=40206d9f --candidate`) times out after 15 min — no PR
  run exists for a push commit. Every main caller carries
  `needs: [plan, policy]` + `needs.policy.result == 'success'`
  (35 occurrences), so one red policy job skips all 36 downstream
  jobs; ci-required fails on `required CI prerequisite policy did not
  pass: failure`. (PR callers in both s1 and s2 have `needs: [plan]`
  with NO policy gate — 0 occurrences — which is why PR legs execute.)

## 6. POST-MERGE PREDICTION (proven by replay, not inferred)

If merged, main goes red with ZERO leg execution — worse in shape than
the PR state, same shape as the current baseline:

- Post-merge tree: pin 40206d9f, schema 2, flip sources. The in-CI
  policy job (setup rev = BASE_PIN = 40206d9f; pin == base so the
  `--check` fast path applies) runs the 40206d9f binary's
  `--plain --check` against the tree. Replay with a locally built
  40206d9f binary over /tmp/r2m-v3: **exit 1**,
  `generated files differ: actionlint.yaml, project.toml, ci-main.yml,
  ci-pr.yml, all 5 ci-unit reusables, release.yml,
  generator-state` (11 files) — the flip's own emitter changes
  (24d1df91, 571 insertions across lib.rs/s2) are not in the pin.
- `--check` fails → candidate path → polls `ci-pr.yml/runs` for
  head_sha = merge commit → no PR runs on push → 15-min timeout →
  in-CI Policy FAILS → all 35 `needs.policy`-gated callers +
  prepare-cargo SKIP → ci-required fails on the policy prerequisite →
  Control/Required mirrors. Net: Planning green; Policy, ci-required,
  Control/Required red; every unit leg skipped.
- Recovery shape (not attempted): a follow-up pin-bump PR (pin =
  merge HEAD) would go green — its standalone Policy runs a base
  validator that knows `trust`, and `--check` would match. But the
  merge itself cannot be green: the flip PR's own Policy failure is
  unfixable inside this PR (no pin value makes the a6fa8d4a validator
  parse `trust`), so merging means knowingly landing red main behind
  a red required check of the PR's own making.
- Rollback path (unchanged): single `git revert` of the merge restores
  the s1 config/tree and the old pin cleanly.

## Per-item scorecard

1. UPDATE INTEGRITY — PASS (ancestor ✓, MERGEABLE ✓, pin exact ✓,
   product triple-linked to 40206d9f ✓, merge = mechanical + theirs ✓,
   DCO ✓).
2. SELECTION FIX IN RENDERS — PASS (11/11 SELECTION_UNITS on the CSV
   channel, 299/299 lines classified, needles stable, no weakenings).
3. REGEN — PASS (force → 21 files clean; dry-run 0).
4. GATES — PASS (1709 + 2209 + 6, clippy/fmt/actionlint clean).
5. CI — FAIL (blocking): 17/17 github green; velnor/admission + rollups
   = excusable environmental baseline (same set #932 merged with);
   Policy red = PR-caused schema skew (base a6fa8d4a validator cannot
   parse PR-introduced `trust`), proven distinct from baseline
   (#932 Policy green via candidate path; main red is pin-stale drift
   + push-timeout, a different mechanism).
6. POST-MERGE — main all-skip red (pin-check exit 1 replayed); recovery
   requires a follow-up pin-bump; rollback clean.

**VERDICT: HOLD** — merge-ready on every axis except the one that
matters: Policy fails on this PR's own config-schema rename, which the
base Stage-0 validator predates and hard-rejects before the candidate
exception. Unblock requires a validator that can read the new schema
(e.g. a main-side policy change making config load delegate to the
bound candidate, then a fresh pin) — a fix outside this PR's tree, so
this PR cannot green itself by regen. Do not merge until Policy has a
passing path.
