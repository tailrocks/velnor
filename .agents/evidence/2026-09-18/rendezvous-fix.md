# RENDEZVOUS-FIX evidence (bastion campaign, load-bearing class fix)

Date: 2026-09-17. Branch: `fix/policy-candidate-rendezvous` @ e6839cfa
(1 commit on origin/main 33688938; merge-base verified == 33688938 before push).
PR: https://github.com/tailrocks/velnor/pull/918 (OPEN, DO NOT MERGE —
merge needs bootstrap authorization: this PR is itself the trap it fixes).

## Problem (from /tmp/candidate-binding.md, proven by code quotes)

Publisher names the candidate by HEAD closure (`ir.rs` head_closure),
Acquire polls by PIN closure + gates manifest==pin (`lib.rs` Acquire
template), validator gates manifest==HEAD (`policy.rs` wanted) — jointly
satisfiable only when pin..head is closure-clean, so NO render-changing
generator PR can ever land green.

## Fix (generator `lib.rs` Acquire template + regen only; `policy.rs` untouched)

- Poll name + manifest gate derive from
  `velnor-workflow closure --rev="$HEAD_SHA" --candidate` (head commit
  fetched when the checkout lacks it) — the identity publisher and
  validator already use. No `pin_candidate` survives (asserted).
- Same-closure early exit additionally requires tree==pin-render via
  `--plain --check` (zero extra provisioning: same closure makes the
  running base validator the pin's own renderer). A same-closure render
  differ falls through to the candidate path with a diagnostic echo
  instead of stranding the validator with a cleared manifest.
- Digest, self-report, shape, and fork gates unchanged; fork gate still
  precedes any candidate fetch (asserted order: render gate < fork < head
  derivation). Trust-invariant comment updated to the new condition.
- Stale `ir.rs` publisher doc ("pin's candidate closure") corrected to
  head's (doc-only, no render effect).
- Regen via generator only: `ci-policy.yml` + `ci-main.yml` (same embedded
  policy job) + state digests. Render diff verified to contain exactly the
  Acquire change + trust comment, zero drift.

## Tests (stubbed static-template assertions, all in `lib.rs` tests)

- `policy_candidate_step_binds_manifest_to_head_and_exports_it` (renamed
  from `..._to_pin_...`): head derivation, head poll name, no
  `pin_candidate`, head manifest gate + error text, manifest export +
  early-exit clearing, Enforce passthrough, digest + self-report gates,
  manifest accept filter, fork gate retained.
- `policy_acquire_same_closure_exit_requires_pin_render_match` (new):
  `--plain --check` gate, both-conditions echo, old unconditional exit
  absent, fall-through echo, gate ordering.
- `policy_acquire_fetches_head_before_deriving_its_candidate` (new):
  head fetch appears twice (checkout + acquire steps), acquire fetch
  precedes the head derivation.

## Gates (branch @ e6839cfa, worktree /tmp/rvz-fix-wt)

- `cargo test -p velnor-workflow`: 543 passed (488 lib + 2 + 6 + 5 + 9 + 33), 0 failed
- `cargo clippy --all-targets -p velnor-workflow -- -D warnings`: clean
- `cargo fmt --check`: clean; actionlint 1.7.12 over 14 workflows: clean
- contract tests (6) / fmt / clippy: clean
- Acquire script: `bash -n` + `shellcheck -s bash -S warning`: clean

## Clean-clone proof (/tmp/rvz-clean @ e6839cfa)

- `--plain --dry-run`: "0 files would change", exit 0
- `--plain --check --pin-build`: exit 0 via candidate notice
  `a1cbc267215f50a6...` (== `closure --rev=e6839cfa --candidate` ==
  binary `--closure`: three-way agreement)
- Pin-bump safety: candidate(5703db61 pin-bump) == candidate(7341ef4b pin)
  == 98e0ebd5... — head==pin when closure-clean, so pin-bump + consumer
  PRs rendezvous on the same name as before. (#914 live rendezvous cited
  in /tmp/pr916-flow.md:134-136 is the same theorem exercised in CI.)

## CI shape, PR #918 — PREDICTED vs ACTUAL (exact match)

Predicted: Planning green, units green, Policy red (same-closure,
tree!=pin render — the trap this PR fixes).

Actual (runs 35183625387 ci-pr + 35183624969 policy, both completed):
- Control / Planning: PASS (10s). DCO: pass.
- ALL GitHub units PASS, incl. `Rust · velnor-workflow / GitHub` (2m14s:
  regen gate stage-1 match + D19 via candidate exception).
- Candidate artifact published: `velnor-workflow-candidate-a1cbc267215f50a6-Linux-X64`
  — exactly the name the NEW Acquire polls (a1cbc267 proven locally);
  only the base-owned old YAML could not poll it.
- Policy: FAIL in 17s with the trap signature, verbatim:
  `VELNOR_WORKFLOW_CANDIDATE_MANIFEST:` (empty — old unconditional
  same-closure exit) → `FAIL generated-tree: the tree differs from the
  render of velnor-workflow at 7341ef4b...` on state + ci-main.yml +
  ci-policy.yml.
- Velnor-lane Bun/Docker/OpenTofu/Prepare-Cargo: FAIL in 2-3s —
  "Velnor rejected this job ... operational store rejected the sanitized
  admission row", the documented pre-existing infra noise (identical on
  #911/#914/#916, out of scope). ci-required + Control/Required red as
  rollups.

## Next (orchestrator decision, NOT done here)

Merge needs bootstrap authorization (red-merge: Policy red is the
base-ownership circularity this PR exists to break). Post-merge chain
per ledger UNBLOCK CHAIN: pin-bump to the merge commit → green → then
#916 rebased goes green via the NORMAL candidate path.
