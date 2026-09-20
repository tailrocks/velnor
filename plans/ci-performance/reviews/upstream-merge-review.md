# Upstream merge review: package publication and sccache emission

Scope: Velnor `9307861d2668424d27012c3c736d48ab47f9012e` (PR #970 merge) and PR #971 head `311a3d2658657aabca3183a74bcd300d0a158956`, reviewed from the isolated `/tmp/velnor-ci-integration` checkout. This review did not modify generator or generated source.

## PR #970

**PASS for the intended package-release fix.** The merge is based on `e717de39b97558117e7e4f55c20216c8a4e4b1fc` and changes only `crates/velnor-workflow/src/s2/primitives/package_release.rs` (93 lines, 77 additions, 16 deletions). It preserves the existing helpers and changes their ownership boundary:

- lock-retention state and its mark/clear helpers are emitted only in mutating finalizer and immutable-publish fragments;
- lock acquisition does not carry retention state or helper definitions;
- the immutable asset `printf` group begins after all lock and verification helper definitions, so function bodies cannot leak into the asset list;
- the added tests assert both helper absence from acquisition and ordering/count/content of the immutable asset group.

The isolated focused run on the exact merge commit compiled successfully and passed 6 of 7 `publication_` tests. The remaining failure was the known Bash 3.2 parent-ERR-trap case in `rolling_verification_failure_preserves_publication_traps`, not a PR #970 assertion: applying the separate `/tmp/velnor-rollback-fix.patch` made that test pass. The combined focused trap test then passed. The package-release change itself has no import/API conflict; `git diff --check` is clean.

## PR #971

**PASS for the source change; HOLD consumer acceptance until regeneration and full checks.** Current fetched head `311a3d2658657aabca3183a74bcd300d0a158956` is a merge of branch parent `c541b95130676db88cc561eec8c20a84ec188918` onto merged main `9307861d2668424d27012c3c736d48ab47f9012e`. Its effective diff from `9307861d2668424d27012c3c736d48ab47f9012e` is exactly two symmetric files:

- `crates/velnor-workflow/src/primitives/ir.rs`
- `crates/velnor-workflow/src/s2/primitives/ir.rs`

The generated sccache environment block changes three independent `>> "$GITHUB_ENV"` writes into one grouped redirect. This fixes ShellCheck SC2129 without changing values, ordering, conditional MBX gate, or the all-disabled behavior. The focused exact-head scratch run passed both schema tests:

```text
primitives::ir::tests::collapsed_rust_all_disabled_omits_mbx_and_keeps_sccache ... ok
s2::primitives::ir::tests::collapsed_rust_all_disabled_omits_mbx_and_keeps_sccache ... ok
2 passed; 0 failed; 1799 filtered out
```

`git diff --check` is clean and the patch adds no imports. Before merging, regenerate checked-in consumers with the reviewed generator revision, then run the full generator suite, actionlint, and consumer drift checks. The generated workflow must contain the grouped form in both supported schemas; source-only tests cannot prove that.

## Main policy/candidate graph: independent challenge

The existing review in [`policy-candidate-cycle-review.md`](policy-candidate-cycle-review.md) correctly identifies a deadlock, but its minimal edge edit needs one extra constraint. On `ci-main.yml` at the reviewed tree:

- `policy` has no `needs` edge and polls only same-repository `ci-pr.yml` runs for a candidate;
- every ordinary caller, including `github-hosted-rust-velnor-workflow`, has `needs: [plan, policy]`;
- the generator caller also has transitive policy-gated prerequisites: `prepare-cargo` needs `[plan, policy]`, and the runner product it needs is policy-gated;
- `candidate_publish: true` is attached to the generator caller, but `ci-unit-rust.yml` prepares and uploads that product only for same-repository `pull_request`; a main push or main dispatch cannot publish the artifact that `ci-main` policy may poll for.

Therefore removing only the direct `policy` item from the generator caller still leaves a dependency cycle through `prepare-cargo`/runner, while changing the upload condition alone lets an ordinary full validation job run before policy. Increasing the poll timeout or accepting a skipped candidate would be incorrect.

The bounded architecture should introduce a typed candidate-bootstrap product edge. Preferred graph for trusted main push/dispatch:

```text
plan ───────────────▶ candidate-bootstrap ─────▶ policy ─────▶ ordinary callers
  └──────────────────────────────────────────────────────────▶ ci-required
```

`candidate-bootstrap` must be a dedicated hosted, tokenless producer with no policy-gated unit prerequisites. It builds/publishes only when the exact source/configuration closure requires a candidate; otherwise it emits a successful “declared pin is sufficient” receipt. Its manifest must bind source/head SHA, generator closure, configuration/plan digest, producer workflow/run/attempt/job, platform, profile/features, binary digest, and successful completion. `policy` must consume the exact current-run artifact on main push/dispatch and retain the existing exact same-repository PR-run path for pull requests. A missing, skipped, failed, expired, or mismatched producer is a policy failure.

The final required gate must retain `plan`, policy, candidate-bootstrap, and every selected caller as explicit obligations. Candidate-bootstrap is a policy prerequisite, not a replacement for generator checks: the full `rust-velnor-workflow` validation caller remains after policy and must preserve format, lint, tests, and required profile/features. The candidate artifact may be transferred into that caller or reused by a compatible product contract to avoid a second candidate build; it must never replace required validation.

Two feasible alternatives need explicit measurement before selection:

1. Move the existing full generator caller and its entire transitive prerequisite closure before policy only for trusted main events, enable publication on main, and let policy consume the current-run artifact. This avoids a second build but exposes the whole generator check closure pre-policy and is easy to regress when a new dependency is added.
2. Split policy into base preflight and candidate-bound final validation. Run preflight independently, produce the candidate after it, then let ordinary callers and the final gate require the candidate-bound verdict. This preserves a policy decision before ordinary untrusted work but adds a policy phase and should be used only if the typed bootstrap product cannot be isolated.

No alternative may use an artifact by name/latest alone, use the policy job's default-branch SHA as the producer identity, or let a skipped candidate satisfy a differing closure. Add graph fixtures for main push, main dispatch with hosted excluded, PR same-repository, fork PR, equal pin closure, differing closure, producer failure, and expired artifact before accepting the DAG change.
