/goal Finish, land, and live-verify the Velnor CI runtime-products re-architecture, ending with it merged to main and proven fast in production CI.

## Starting state (authoritative, verify before acting)

- Repo: https://github.com/tailrocks/velnor (origin).
- Delivery branch (pushed, up to date): `feat/ci-immutable-runtime-products` at `a10c7b39`
  ("fix(ci): move producer CARGO_HOME to step env..." on top of `1d46826d`
  "feat(ci): consume immutable runtime products...").
- Its base is `8c495870` (the old, now-closed PR #883 head). main has since moved
  ~40 commits ahead; expect drift in `crates/velnor-workflow/src/lib.rs`,
  `primitives/ir.rs`, `policy.rs`, and generated workflows.
- Local worktree with this state: `<redacted-local-path>`
  (branch checked out). Do all work there. Do NOT touch
  `<redacted-local-path>` or any other branches
  except the delivery branch, and main only via the final merge itself.
- Prior session evidence (still valid, do not redo from scratch): 531 tests green,
  `cargo clippy --all-targets -p velnor-workflow -- -D warnings` clean, `cargo fmt --check`
  clean, regen idempotent (`--plain --dry-run` = 0 files), contract tests 6/6 green,
  old-validator transition CLEAR (only staleness-shaped generated-tree findings),
  three falsification rounds closed (final: 8/10 claims PROVEN, 2 HIGHs fixed and
  re-verified), live GitHub accepts all generated workflows.

## What remains

1. **Rebase onto current main.** `git fetch origin main`, rebase the delivery branch
   onto `origin/main`. Conflicts are expected in generator sources and generated YAML.
   Resolution rule: keep the product architecture (closure identity, product-only setup
   action, head-anchored candidate, D19 fail-closed, sparse base checkout,
   MISE_NO_CONFIG lint, digest-first provisioner, context-availability rule) and take
   fleet-side changes everywhere else. NEVER resolve a YAML conflict by hand-editing
   generated files: resolve sources, then regenerate.
2. **Regenerate with the rebased tree.** Build the rebased generator and run
   `./target/debug/velnor-workflow --plain --force`, then confirm
   `--plain --dry-run` reports 0 files. All `.github/workflows/*.yml` and the
   installed setup action must be pure generator output. If fleet changes altered
   templates you depend on (setup action shape, policy_job, candidate_publish_steps,
   runtime_products), adapt the re-architecture to the new template structure rather
   than reverting fleet work — but do not weaken any security invariant to do so
   (same-repo gates, digest-before-exec, tokenless exec points, attestation).
3. **Re-run every gate on the rebased tree:** `cargo test -p velnor-workflow`,
   standalone contract tests in `crates/velnor-workflow-contract`, clippy as above,
   fmt check, actionlint on changed workflows if available. All green, no exceptions.
4. **Re-prove the old-validator transition** on the final tree: build the validator
   from `8c495870` into /tmp (never in the worktree), run
   `policy --workflow-root <worktree> --base-revision <[generator] revision>`
   with an independent subagent. Required: CLEAR (only staleness-shaped
   generated-tree findings; any other failing rule is a BLOCKER).
5. **Spot re-falsify the security-critical deltas** introduced by the rebase only
   (independent subagent, read-only): confirm zero `cargo install/build` in
   ci-pr/ci-policy/setup-action, zero `--pin-build` in YAML, candidate still
   head-anchored and digest-bound pre-exec, fork gates intact. Full re-audit is NOT
   required if the rebase touched none of those paths — prove that instead.
6. **Open the PR** with `gh pr create` (base `main`, head the delivery branch):
   title `feat(ci): consume immutable runtime products instead of building velnor-workflow in CI`;
   body must state the architecture (Stage-0 closure-keyed releases + Stage-1 candidate),
   the security model (PRT confinement + explicit safe-and-unavoidable justification),
   gates run, and that it supersedes the closed #883 approach. Conventional commits,
   DCO signoff (`-s`) on all commits.
7. **Watch CI and capture LIVE timings.** Wait for the PR's Planning and Policy jobs;
   record per-step timings from the GitHub API and compare against the pre-change
   baselines (Planning 223s with 120s+91s builds; Policy 221s with 105s+99s builds)
   and the projected floors (Planning ~9–17s, Policy ~14–22s). Requirement: zero
   cargo invocations in consumer-step logs, setup steps complete in seconds, policy
   compute ≈1s. If any consumer compiles, stop and fix at the generator level —
   do not merge around it. Also confirm no "Invalid workflow file" runs appear.
8. **Merge** per repo convention (check how recent merged PRs landed — squash vs merge;
   follow it; use `gh pr merge`). Then verify post-merge: the `ci-runtime-products.yml`
   `push: [main]` run executes, mints the first `velnor-workflow-runtime-v1-*` release
   with manifest + attestations, and its smoke test passes. If the producer fails,
   fix forward on the delivery branch in a follow-up (do not commit directly to main).
9. **Final live proof.** After the producer release exists, confirm end-to-end on a
   subsequent PR (or an empty commit to a scratch branch + PR): Planning + Policy run
   fully from products. Record those timings as the closing evidence.

## Rules

- No hand-edited generated YAML, ever; generator → regen → verify, always.
- No weakening of policy rules, branch/ruleset checks, or trust confinement to make
  the rebase easier. No mutable `latest`, no unverified artifacts, no cache-dependent
  correctness, no merge-SHA product identity.
- Use subagents aggressively; the transition proof and security spot-check must come
  from agents that did not write the rebase resolutions.
- If the rebase uncovers a genuine semantic conflict where fleet work and this
  architecture cannot both hold, stop and report the conflict precisely — do not
  silently drop either side.

## Deliverable

Report: rebase conflicts and their resolutions, gate outputs on the final tree,
transition verdict, PR URL, live before/after per-step timing tables from real CI
runs, producer release URL + attestation evidence, merge commit, and any remaining
non-instant step with justification. Completion = merged to main with live CI proving
Planning + Policy consume products instead of compiling.
