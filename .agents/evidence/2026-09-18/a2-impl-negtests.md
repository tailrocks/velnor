# A2 G6+G7 implementation evidence — consumer negatives author

- Branch: `feat/a2-consumer-negatives` (from `docs/bastion-final-plan` @ `38ffbfd7`)
- Commit: `4284f246` (signed off, pushed to origin)
- Files: `crates/velnor-workflow/src/consumer_negatives.rs` (new, ~1490 lines,
  `#[cfg(all(test, unix))]`), `crates/velnor-workflow/src/lib.rs` (+2 lines:
  module declaration)

## What was built

Executable consumer verification suite closing G6+G7. Each case runs the REAL
scripts under `bash` — setup-action step bodies extracted from the shipped
`action.yml`, the rendered Velnor provisioner
(`workflow_pinned_policy_runtime_velnor`), the candidate-acquire verification
tail (`policy_candidate_step`) — with fixture git history and:

- stub `gh`: serves fixture release/attestation/run answers, logs every call,
  REFUSES `attestation verify` without the pinned `--owner` +
  `--signer-workflow` flags (a passing run proves the pin held);
- logging `jq` shim over the REAL binary (accept filters evaluate for real;
  the log proves they ran — stubbing jq would weaken every manifest proof);
- real `git` over fixture history only; expected closures computed in Rust via
  `crate::closure` (script/Rust agreement asserted, never assumed);
- `install` shim for exactly `install -Dm0755` (macOS lacks `-D`); its log
  proves whether the slow-path install ran;
- hermetic: default `GITHUB_SERVER_URL` points at a nonexistent local path;
  lookup-confusion cases use local-path fetch remotes. No network possible.

19 tests, all passing:

G6 setup action: missing product (names revision + closure + producer, fails
before any trust), wrong digest, wrong manifest ×7 (incl. same-16-prefix
closure proving the tag is locator-only), untrusted signer (asset AND manifest
subjects; log proves the rejecting gate pinned owner+workflow), poisoned-cache
re-verification, self-report mismatch, no-checksum-input declaration.
G6 Velnor provisioner: same six rejections through the rendered provisioner,
plus slot-reuse positive control (manifest downloads fresh, zero asset fetch,
zero install, bytes untouched) and self-report-as-post-install-gate proof.
G6 candidate (PR-checksum-as-trust): digest recomputed (mismatch rejects);
self-consistent manifest+bytes with foreign closure rejected by the pin
binding ("not the pin's candidate", digest gate demonstrably passed);
positive control exports both env bindings.
G7 cold consumer: empty cache → closure/download/verify/PATH all succeed;
exactly 2 attestation verifications (asset+manifest, pinned flags), accept
filter ran on both paths, full-64-hex closure threaded end to end,
unconditional verify step asserted in the composite (zero waivers).

## Verification (this session, final code state)

- `cargo test -p velnor-workflow`: ALL GREEN — lib 505 passed / 0 failed
  (incl. 19 new `consumer_negatives::*`), plus integration targets
  2+6+5+9+33 passed, 0 failed.
- `cargo clippy -p velnor-workflow --all-targets --all-features -- -D warnings`:
  clean (3 lints found during development, all fixed).
- `cargo fmt -p velnor-workflow -- --check`: clean.

## Findings for other bundles (not fixed here — out of scope)

1. The Velnor provisioner's `git fetch` carries no `-C "$CHECKOUT_PATH"` (lib.rs
   `workflow_pinned_policy_runtime_velnor`): it fetches into the step's cwd,
   which only equals the checkout by CI coincidence. The suite runs it with
   cwd=checkout to mirror CI. Recommend the G3 owner add `-C` when touching
   that line. (Pre-fix, one test run fetched fixture objects into the worktree
   object store as dangling objects — harmless, auto-expire; working tree
   verified clean, history untouched.)
2. Candidate-acquire executes the verification tail only (download → binding);
   the polling head is liveness, not trust — documented in module docs.
3. Signer-ref pinning (G2) is a separate bundle: N4 here proves the current
   `--owner` + `--signer-workflow` gate rejects and is actually evaluated.
