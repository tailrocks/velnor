# PR #916 FIX evidence — feat/a2-producer-revision @ 1bee4f23

## Head

- Branch `feat/a2-producer-revision` pushed to origin at
  `1bee4f237277fe32194c313b6bdaffdfd43eb7dc` (local == origin, worktree clean).
- New commit (signed, no amend): `fix(pr-916): pure phase-1 generator PR
  (revert self-bump to unmerged rev)`. NOT merged, per instructions.
- Work done in `/private/tmp/a2-producer-worktree` (pre-existing worktree on
  this branch) to avoid disturbing the main checkout's in-progress merge.

## Restructure (as prescribed)

Reverted `f4d46b3b` (pin bump to unmerged `51e635af`) in full: pin back to
`7341ef4bdf750c1fbe419e94fb3848c5b8dde718`, all pin-only regens reverted.
Net diff vs base `33688938` is 3 files:

- `crates/velnor-workflow/src/primitives/runtime_products.rs` (the generator
  change, kept),
- `.github/workflows/ci-runtime-products.yml` (93 lines: genuine new-generator
  render — revision manifest, default-branch guard, verify-before-create),
- `.github/ci/.github-actions-generator-state` (1 hash line for the above).

The render DOES differ under the new generator, so per the brief's fallback
the regen diff is kept MINIMAL (2 files) with the pin on old. Dropping it
would break the `--check` fixed point (proven below: `--check` exit 1,
"generated files differ", on the reverted tree).

## Task gates (all green)

In `/private/tmp/a2-producer-worktree` @ 1bee4f23:

- `cargo test -p velnor-workflow`: 491 + 2 + 6 + 5 + 9 + 33 unit/integration
  tests pass, 0 failed (546 total, 8 binaries incl. doc-tests).
- `cargo clippy -p velnor-workflow --all-targets`: exit 0, no warnings.
- `cargo fmt -p velnor-workflow --check`: exit 0.
- `actionlint`: exit 0.
- `velnor-workflow --plain --dry-run`: exit 0, "0 files would change".
- `velnor-workflow --plain --check`: exit 0, "Generated files are current",
  plus the designed notice: tree matches the CANDIDATE render
  (`7dcabc83ea2c2a6b…`, same closure the red diagnosis saw in the candidate
  artifact name), not the declared pin; bump revision after merge.

Clean clone `/tmp/pr916-clean` (fresh `git clone` from origin, branch head,
`mise trust` for the new dir): `--plain --dry-run` exit 0
("0 files would change"), `--plain --check` exit 0 (same candidate notice,
same closure — deterministic). Note: local `--check` resolves the pin
renderer from machine-global state (PATH binary too old, falls through to the
`--pin-build` cache at `$TMPDIR/velnor-workflow-policy-7341ef4b…/bin`,
revision/closure verified as the pin); a machine without that cache would
fail closed with "no renderer provisioned". CI provisions via setup action.

## CI ground truth on 1bee4f23 (falsifies the brief's Policy premise)

The red diagnosis predicted "same-closure path → tree byte-identical →
green". That assumed a render-neutral generator. The render is NOT neutral,
and CI proves the consequence:

- `Control / Planning`: SUCCESS (chicken-and-egg fixed: old-pin product
  exists; the red-diagnosis Failure 1 is gone).
- `Rust · velnor-workflow / GitHub`: SUCCESS (new generator compiles, tests
  pass, candidate publishes).
- `DCO`: SUCCESS.
- `Policy` (run 35175919250): FAILURE —
  `FAIL generated-tree: the tree differs from the render of velnor-workflow
  at 7341ef4b…` naming exactly the 2 kept regen files.
- Velnor-lane `FAILURE`s also observed (`Docker/Documentation/OpenTofu /
  Velnor`, `Prepare Cargo/prepare-cargo`); outside this diff's blast radius
  (only velnor-workflow source + runtime-products workflow touched; all
  GitHub-lane units green). Run still in progress when checked, failed-job
  logs unavailable — confirm separately (suspect self-hosted flake).

Root cause of the premise error (verified in code AND by execution): local
`--check` runs the NEW binary, which is manifest-exempt and matches the
tree as Candidate → exit 0. CI Policy runs the OLD base product in the
same-closure path (pin == base pin → Acquire exits early, no candidate
manifest, no pinned pointer), where `render_with_candidate` has only the
base binary whose release closure can never equal the tree's candidate
closure (`policy.rs:1469-1545`, `closure.rs:108-177`: release vs candidate
namespaces). The candidate exception is dead code in that path: tree must
equal the pin render byte-for-byte or Policy fails with Differences.
Pre-push simulation with the real pin binary
(`$TMPDIR/velnor-workflow-policy-7341ef4b…/bin/velnor-workflow`, rev+closure
verified) predicted the CI log BYTE-IDENTICALLY, including the two files.

Non-precedent note: gate-lanes `2773e123` (old pin + new render) never faced
PR CI — zero check-runs, no PR attached, merged directly to main. And
mainline `f16cc51a` (that same state) shows `Publish runtime products |
failure` / `Control / Required | failure`. No evidence any old-pin+new-render
state ever passed Policy.

## Remedy (needs parent decision; NOT pushed)

The only Policy-passing phase-1 shape for a render-changing generator is
source-only (Option C): revert the 2 regen files to base too, leaving ONLY
`runtime_products.rs` vs base. Proven locally with the real pin binary
(scratch tree `/tmp/pr916-C-wt`, left in place for inspection):
`policy: 11 rules, 0 failed`, generated-tree via Pin. Planning stays green
(same old-pin setup). Cost, measured: new-binary `--check` exits 1
("generated files differ: ci-runtime-products.yml, state") — BY DESIGN,
the tree is deliberately stale until the phase-2 pin bump adopts the new
render; `--dry-run` still exits 0 (reports "2 files would change"). No CI
job runs `--check`/`--dry-run` (local-only gates; verified by grep), so
nothing in CI observes the staleness. Phase 2 is unchanged (bump pin to the
phase-1 merge + regen → candidate path, HEAD source == pin source → green).

Commands, if approved (from a worktree on this branch):
`git checkout 33688938 -- .github/workflows/ci-runtime-products.yml
.github/ci/.github-actions-generator-state && git commit -s -m "…" && git push`.

## Decision record

Stopped at the prescribed structure (1bee4f23 pushed) rather than pushing
Option C unasked: the brief's fallback explicitly directed keeping the
minimal regen diff, the turn contracts a single commit + evidence + return,
and the branch is under do-not-merge caution. The falsified premise, CI
proof, and exact remedy above are the handoff for the parent's call.
