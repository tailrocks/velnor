# A1 stalerev: run 35136207272 attempt-2 root cause + proposed fix

## Failing site (exact)

`crates/velnor-workflow/src/closure.rs:152-157` (`closure_of_tree`):
any `git ls-tree -r <rev>` failure maps to
`revision {rev} is not a commit of {repo}`.

Reached in `--plain --check` via one path only:
`lib.rs:4800 verify_declared_pin_renders_tree`
→ `policy.rs:1425 regenerate_and_compare`
→ `policy.rs:1438 expected_closures(checkout, pin)?`
(`PinSource::Checkout` arm uses `?`, fail-closed).
The sibling string at `policy.rs:1347` says "of the audited checkout";
the observed `of /home/runner/work/velnor/velnor` (a real path) matches only
`closure.rs:154`. `checkout` = `Checkout::Local` workspace root
(`lib.rs:6692`), no worktree/temp clone involved.

## What the run was (ground truth from `gh`)

- Run 35136207272 = PR tailrocks/velnor#904
  (`feat/ci-immutable-runtime-products`, same-repo, public), merged later.
- Attempt-2 failure = job `Rust · velnor-workflow / GitHub`
  (ID 104931565302). Velnor lane was `skipped` (deps).
- Run `headSha` = `62a74bf5` = "chore(ci): bump D19 pin to 48a66ad7",
  whose parent is exactly `48a66ad7` ("fix(ci): drop rejected
  --signer-repo…"). Both are PR-#904-side commits (verified in
  `4dc8da58^2` stack); base = `3353310c`.
- So at run time the tree's declared D19 pin (`48a66ad7`) existed ONLY on
  the PR branch, one commit below head.

## Why a valid revision was "not a commit"

1. Unit jobs check out shallow: `Checkout … ref: inputs.head_sha` with no
   `fetch-depth` (= depth 1) at the merge commit (`ir.rs:4036-4040` and the
   rendered `ci-unit-rust.yml`). The merge contains the pin via the head
   side, but depth 1 materializes only the merge commit itself.
2. The run's `ci-unit-rust.yml` (rendered from the PR tree) had NO
   "Fetch D19 pin history" step — the step-list from `gh` confirms it.
   That step was authored AFTER this failure as `d129fb89` on this same
   PR branch ("…8e6edac1 bump to d129fb89" follows in the stack), then merged.
3. `--check` phase 1 (drift) passed — the PR tree was self-consistent with
   its own (pre-`d129fb89`) generator — then phase 2 called
   `expected_closures(checkout, 48a66ad7)` → `git ls-tree -r 48a66ad7`
   → absent object → the observed error, exit 1.

Ruled out, with evidence:
- PR merge-ref vs head: merge side contains the pin; absence is purely the
  depth-1 boundary, not the wrong ref.
- Worktree: `Checkout::Local` canonicalizes the workspace; error path is the
  real workspace.
- `mbx` sandboxing: `mbx run` is a cargo build-cache wrapper
  (`mbx --help`; `ToolRequirement::MrBoxington`), not a filesystem
  snapshot — the check sees the real workspace `.git`.

## Repro (real binary, both directions)

- `git clone --depth 1 file://… /tmp/repro-shallow`, then
  `velnor-workflow closure --rev 48a66ad7… --repo /tmp/repro-shallow`
  → `error: revision 48a66ad7… is not a commit of /tmp/repro-shallow`, exit 1.
- `git -C /tmp/repro-shallow fetch --depth 1 origin 48a66ad7…` → same
  command prints digest `f1f88c20…`. Fetch-if-missing is the mechanism.

## Why the architecture allowed this class

`closure_of_tree` assumes a full-history object store, but unit checkouts
are deliberately shallow. The assumption is satisfied not by the tool but
by per-lane shell snippets emitted by the generator — a mechanism that:

- is forgettable per lane (GitHub lane had none → this failure);
- disagrees on WHICH rev to fetch: GitHub fetches the LIVE pin parsed from
  `.github-gen/velnor-workflow.toml` (`d129fb89`); Velnor fetches
  `PINNED_REVISION` baked at render time (`lib.rs:4520`), equal to the live
  pin only via phase-1-consistency coupling, not by construction;
- duplicates pin parsing (`sed` in YAML vs Rust config parsing) and remote
  addressing in two places that can drift independently.

## Residual gaps after `d129fb89` (verified in current source)

1. Velnor lane still fetches the baked render-time pin, not the live declared
   pin (`ci-unit-rust.yml:709` `PINNED_REVISION: 7341ef4b…`, no other fetch
   in `verify-velnor`). Correct today only because drift would fail first.
2. Velnor lane cannot validate PR-side pins at all: the provision step needs
   a published release for the pin's closure, but `ci-runtime-products.yml`
   runs on `push: [main]` + dispatch only ("…builds it after merge").
   Any PR whose pin is PR-side fails closed at Provision on Velnor.
   (Unobserved in attempt 2 — Velnor was skipped — same bug class.)

## Proposed source fix (not implemented; no edits made)

P1 — core, structural (removes the enabling condition):
In `policy.rs::regenerate_and_compare` (Checkout arm, before
`expected_closures`), ensure presence in the tool instead of in lane YAML:
```rust
if !commit_exists(checkout, pin) {
    // git fetch --no-tags --depth 1 origin <pin>; on failure or still
    // absent, fail loud naming pin + shallow state + remediation
    // (fetch-depth: 0 / rebase), reusing the missing_commit_reason style.
}
```
Keep strict closure verification (no downgrade to the revision fallback).
Use remote name `origin` (works in CI checkouts and local clones; better
than the snippet's hardcoded `$GITHUB_SERVER_URL/$GITHUB_REPOSITORY`).
Do NOT gate on `CARGO_NET_OFFLINE` (cargo-scoped; these jobs already
`git fetch` + `gh release download`). Offline + full clone still never
fetches (`commit_exists` short-circuits), so local behavior is unchanged.

P2 — unify the Velnor provision step on the live pin:
`workflow_pinned_policy_runtime_velnor` (`lib.rs:4520`) should read the pin
from `.github-gen/velnor-workflow.toml` at runtime (same `sed` as the
GitHub step) for its fetch + closure/tag computation, instead of the baked
`PINNED_REVISION`. Single source of truth = the audited tree. (Does not
fix gap R2 — PR-side pins still lack a release product; that needs a
PR-scoped product build or a source-build fallback for same-repo runs,
separate change; name it, don't bundle it.)

P3 — cleanup: with P1, delete the now-redundant "Fetch D19 pin history"
emission in `ir.rs:4445-4475` + its test + regen (tool is the single
fetcher; error text must stay as diagnostic as the snippet path).

Tests to add with the fix:
- Fixture: two-commit repo, depth-1 clone of the tip via `file://`,
  declared pin = parent; `--check`/ `regenerate_and_compare` passes after
  self-fetch (and fails loud with remediation when the remote lacks the pin).
- Update `github_lane_fetches_pin_history_for_check_running_units` for P3;
  add a render test that the Velnor provision step reads the live pin (P2).
- Re-run: `cargo test -p velnor-workflow` + the byte-for-byte
  `checked_in_workflows_match_the_generator_byte_for_byte` after regen.
