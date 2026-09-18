# V-RENDEZVOUS: independent review of PR #918 (bastion campaign)

Date: 2026-09-17. Reviewer: rendezvous reviewer (validation ONLY, no edits, no merge).
Inputs read first: /tmp/candidate-binding.md, /tmp/rendezvous-fix.md, ledger IMPOSSIBILITY RESULT.
Subject: https://github.com/tailrocks/velnor/pull/918, branch fix/policy-candidate-rendezvous @ e6839cfa, claimed base main 33688938.

## VERDICT: CERTIFIED (content) — merge executes only after the mechanical 2-line conflict resolution below

The fix is exactly what the evidence claims and all gates reproduce in scratch.
One extrinsic landing blocker: origin/main moved 33688938 -> 08ea1b07 (#917 merged
after #918 was pushed), so #918 now conflicts with current main in the state digest
file and GitHub will refuse the merge until resolved. The resolution is fully
mechanical, its content is determined below, and the resolved tree passes every gate
in scratch (754/754 lib tests, regen drift zero, dry-run/check zero). This is NOT a
fix defect — no rework of #918 is needed.

## 1. Base + scope (verified)

- `git rev-parse origin/fix/policy-candidate-rendezvous` = e6839cfa...; merge-base
  with origin/main = 33688938297eee3933997dbbf368ee20fb1779d4 EXACTLY. Single commit
  on base: `e6839cfa fix(policy): rendezvous the candidate path on the head closure`.
- PR head still e6839cfa on GitHub (no pushes since evidence recorded); state OPEN.
- Diff file list (base..head): ONLY
  `.github/ci/.github-actions-generator-state`, `.github/workflows/ci-main.yml`,
  `.github/workflows/ci-policy.yml`, `crates/velnor-workflow/src/lib.rs`,
  `crates/velnor-workflow/src/primitives/ir.rs`. No campaign code.
  `policy.rs` untouched (empty diff) — validator `wanted`=HEAD leg unchanged, so the
  mainline candidate exception still fails closed exactly as before.

## 2. Fix mechanics (verified in diff, /tmp/v-rvz-wt @ e6839cfa)

- Acquire polls `head_candidate="$(velnor-workflow closure --rev="$HEAD_SHA" --candidate)"`
  with fetch-if-needed (`git cat-file -e` guard + fetch, matching the checkout step);
  manifest gate is `manifest_closure == head_candidate` with head-naming error text.
- Early exit now requires BOTH same-closure AND tree==pin-render via
  `if velnor-workflow --plain --check; then` (same closure makes the running base
  validator the pin's own renderer — sound under the closure completeness the whole
  system already assumes); a differ falls through with a diagnostic echo.
- Fork gate retained and still precedes any fetch (render gate < fork < head
  derivation, asserted by test); digest, self-report, shape, same-repo gates unchanged.
- `pin_candidate`: zero occurrences in either rendered workflow; the 2 lib.rs hits are
  the test + doc asserting its absence. ir.rs change is 1 doc line (pin's -> head's).
  ci-main.yml hunk is byte-identical in shape to ci-policy.yml's. Trust comment updated.
- Render diff = Acquire change + trust comment + state digests only; dry-run in scratch
  worktree: "0 files would change", exit 0 (reproduced).

## 3. Gates rerun in scratch worktree /tmp/v-rvz-wt (all reproduce evidence)

- `cargo test -p velnor-workflow`: 543 passed (488 lib + 2 + 6 + 5 + 9 + 33), 0 failed —
  exact match. The 3 rendezvous tests pass by name
  (`..._binds_manifest_to_head_and_exports_it`,
  `..._same_closure_exit_requires_pin_render_match`,
  `..._fetches_head_before_deriving_its_candidate`).
- Contract crate (standalone, `crates/velnor-workflow-contract`, not a workspace member):
  2 + 4 = 6 passed.
- `cargo clippy --all-targets -p velnor-workflow -- -D warnings`: clean.
  `cargo fmt --check`: clean. actionlint 1.7.12: clean.
- Acquire script (extracted block): `bash -n` + `shellcheck -s bash -S warning`: clean
  (both branch and merged versions).

## 4. Closure proofs (independent, built binary in scratch)

- candidate(e6839cfa) = a1cbc267215f50a6... == binary `--closure` (built at e6839cfa)
  == CI-published `velnor-workflow-candidate-a1cbc267215f50a6-Linux-X64` (confirmed via
  Actions API) == local `--plain --check --pin-build` candidate notice ("tree matches
  the candidate render (a1cbc267...), not the render of the declared pin 7341ef4b",
  exit 0). Four-way agreement: the NEW Acquire polls exactly the published name.
- Pin-bump safety: candidate(5703db61 "chore(ci): bump D19 pin to 7341ef4b") =
  candidate(7341ef4b) = 98e0ebd5... (identical) — head==pin when closure-clean, so
  pin-bump + consumer PRs rendezvous on the same name as before. Consumer (non-generator)
  PRs still early-exit: tree==pin-render now PROVEN by --check instead of assumed.
  #917's green Policy (3m37s, candidate path) is live precedent for the closure-clean
  theorem; #914 rendezvous cited in evidence is the same.

## 5. Actual PR CI shape (matches prediction + authorization scope)

- Predicted: Planning green, units green, Policy red-only (trap).
- Actual (runs 35183625387 + 35183624969): Control/Planning PASS, DCO pass, ALL GitHub
  units PASS incl. `Rust · velnor-workflow / GitHub` 2m14s; Policy FAIL 17s with the
  verbatim trap signature (empty `VELNOR_WORKFLOW_CANDIDATE_MANIFEST` -> `FAIL
  generated-tree: the tree differs from the render of velnor-workflow at 7341ef4b...`
  on state + ci-main.yml + ci-policy.yml). "Mainline exception still fails" confirmed.
- Extra reds: Velnor lanes (Bun, PrepareCargo, Docker, OpenTofu) 2-3s fails with
  "operational store rejected the sanitized admission row" + `Control / Required` +
  `ci-required` rollups. Pre-existing infra noise: identical set on #916 (checked) and
  on MERGED #917 (which merged with that set + Docs/Velnor; #918's docs passed).
  So #918 red set = merged precedent + Policy trap ONLY — within the authorized
  "Policy-red-only + all-else-green" scope (Velnor lanes fail closed pre-execution on
  every PR and are non-blocking by precedent).

## 6. Landing blocker (extrinsic): state-digest conflict with current main

- `git merge` of e6839cfa into 08ea1b07 conflicts ONLY in
  `.github/ci/.github-actions-generator-state` (both sides refreshed the ci-main.yml /
  ci-policy.yml digests; #917 changed pins, #918 changed Acquire). All other files
  auto-merge coherently (verified: merged Acquire = new head-based body + #917 pins
  0e6645af + #917 pin-fallback lines, gate order intact).
- Resolvability PROVEN in scratch /tmp/v-rvz-merge (merged tree, conflicts resolved via
  generator regen per standing rule: placeholder + `git add -A` + `--plain --force`):
  regen changed ONLY the 2 state digest lines (zero YAML drift — textual merge already
  byte-identical to merged-generator render); resolved tree: lib tests 754/754 pass,
  `--plain --dry-run` 0 files, `--plain --check` exit 0, actionlint + Acquire lint clean.
- Exact resolution content (valid for merge of 08ea1b07 + e6839cfa only; re-resolve via
  regen if main moves again):
  `.github/workflows/ci-main.yml\tc34b56830e832e15`
  `.github/workflows/ci-policy.yml\tc090093d9ef370b2`
  (inputs section keeps HEAD side: config b18960683fdb077d, scan fc874ae71dabcd22,
  generator 50 — regen-verified.)

## 7. Merge-readiness under the one-time bootstrap authorization

- Scope check: this PR only, Policy-red-only, immediate pin-bump after — all satisfied
  (§5). The conflict resolution adds only main's already-merged #917 content + the 2
  regen digests; fix content lands intact, within scope.
- Required landing sequence (orchestrator, NOT done here): resolve the conflict by
  merging main into #918 (new commit, no history surgery) + `git add -A` + generator
  regen + push, confirm CI re-runs to the same shape (Policy red trap, all GitHub green),
  then bootstrap-merge under the recorded authorization, then the immediate green
  pin-bump PR to heal main. Alternative within authorization: local merge + regen
  resolution + push to main directly.
- Post-merge chain per ledger holds: pin-bump to the merge commit goes green via the
  fixed rendezvous (§4), then #916 rebased goes green via the NORMAL candidate path.

## Verdict rationale

MISMATCH would assert the PR or its evidence is wrong — everything reproduced exactly,
so MISMATCH is the wrong signal. The sole blocker is main-drift conflict whose
resolution is mechanical and fully verified in scratch. CERTIFIED with the hold above.
