# INTEGRATION-14: bootstrap shepherd for PR #918 (bastion campaign)

Date: 2026-09-17. Scope: ONE-TIME user-authorized bootstrap merge of PR #918
ONLY (ledger IMPOSSIBILITY RESULT entry). Inputs read first:
/tmp/v-rendezvous.md (CERTIFIED with hold), /tmp/rendezvous-fix.md, ledger
authorization, /tmp/candidate-binding.md.

## 1. Main drift under the task (handled, not aborting)

Task named origin/main 08ea1b07, but live origin/main had moved to `dead5ecb`
("chore(ci): bump D19 pin to 08ea1b07 after PR994-capabilities merge" — pin
literals + tracking hashes only, closure unchanged per its message). Merging
stale 08ea1b07 would not have resolved the conflict vs current main, so the
shepherd merged CURRENT origin/main `dead5ecb`, per the certification's
"re-resolve via regen if main moves again" instruction.

## 2. Conflict resolution (scratch worktree /tmp/i14-wt, detached @ e6839cfa)

- `git merge origin/main`: conflict ONLY in
  `.github/ci/.github-actions-generator-state`; ci-main/ci-policy/lib.rs/ir.rs
  all auto-merged. Merged Acquire = new head-based body + dead5ecb pins
  (08ea1b07), zero `pin_candidate` in rendered workflows.
- Resolution per standing rule: placeholder + `git add -A` + rebuilt-generator
  `--plain --force` regen. Regen's unstaged diff touched ONLY the state file
  (ZERO YAML drift). Regen'd state vs main's state: EXACTLY the 2 digest lines
  (ci-main `60a8afb95ea5163e`, ci-policy `3ef3642a0ac6a481`); inputs + all other
  outputs identical, `.github-gen/velnor-workflow.toml` byte-identical to main.
- Gates at resolved tree (before commit): `cargo test -p velnor-workflow` all
  pass (754 lib + all integration suites, 0 failed — 754 matches the reviewer's
  merged-tree count); 3 rendezvous tests pass by name; clippy `-D warnings`
  clean; `cargo fmt --check` clean; actionlint 1.7.12 clean; `--plain --dry-run`
  "0 files would change" exit 0; `--plain --check --pin-build` exit 0 with
  candidate notice ("tree matches the candidate render (a1cbc267...), not the
  render of the declared pin 08ea1b07..."); merged Acquire `bash -n` +
  `shellcheck -s bash -S warning` clean.
- Committed with `git commit -s`: `933239b2` "Merge origin/main (dead5ecb)
  into fix/policy-candidate-rendezvous". Pushed
  `HEAD:fix/policy-candidate-rendezvous` (e6839cfa..933239b2).
- candidate(933239b2) = 6a5f8d0f3b2bf48e... (local record; CI publishes by the
  merge-commit identity under the fixed head-rendezvous).

## 3. Disclosure comment

Posted https://github.com/tailrocks/velnor/pull/918#issuecomment-5709785763
(body /tmp/i14-disclosure.md): authorization, impossibility summary, predicted
CI shape, pin-bump to follow.

## 4. CI re-run shape verification (EXACT match, no abort)

- Policy run 35188713563: single job "Policy" FAIL with the verbatim trap
  signature ONLY: empty `VELNOR_WORKFLOW_CANDIDATE_MANIFEST` ->
  `FAIL generated-tree: the tree differs from the render of velnor-workflow
  at 08ea1b07...` (pin updated per dead5ecb — correct).
- ci-pr run 35188715787 (completed/failure): Control/Planning PASS; DCO pass;
  ALL GitHub units PASS incl. `Rust · velnor-workflow / GitHub` (3m42s);
  failure set = {Bun/Velnor, Docker/Velnor, Documentation/Velnor,
  OpenTofu/Velnor, Prepare-Cargo, ci-required, Control/Required} — BYTE-IDENTICAL
  to merged-#917 run 35184057652's set (Docs/Velnor newly red vs #918's old run
  but inside the #917 precedent; its cause is the same admission-noise
  signature: "operational store rejected the sanitized admission row", 3s,
  fail-closed pre-execution).
- Prediction check: Planning green ✓, all GitHub units green ✓, Policy red
  trap-only ✓, Velnor noise == #917 precedent set ✓, nothing else red ✓.

## 5. Merge (the authorized bypass, no other bypass anywhere)

`gh pr merge 918 --merge --admin` → MERGED at 2026-09-17T06:22:15Z.
Merge commit: `033ab54675c5ec9386f5da3f7fdf2b4bc7217a3c`
(parents dead5ecb + 933239b2).

## 6. Post-merge main verification

- `git rev-parse origin/main` = 033ab546; PR state MERGED.
- Main CI started on 033ab546: CI/main in_progress (35189526626), Runtime
  products in_progress (35189525921), Preview completed/failure (35189526406,
  "Resolve preview identity" — same pre-existing Preview red as before; main
  was already red at dead5ecb, out of scope).
- NOT done (separate task): immediate pin-bump PR to the merge commit, then
  #916 rebase per ledger chain.
