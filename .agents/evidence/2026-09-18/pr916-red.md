# PR #916 RED diagnosis — feat/a2-producer-revision @ f4d46b3b

## Verdict (one line)

Both failures share a single root cause: **#916 self-bumps the D19 generator pin to its own
unmerged commit `51e635af` in the same PR as the generator change**. No runtime product can
exist for an unmerged revision (the producer publishes from `main` only, post-merge), so
`Control / Planning` dies in setup and `Policy` starves waiting for a candidate artifact the
dead sibling run never publishes. The producer-split logic itself is sound; the pin bump is
the defect. Not pre-existing on main.

## Failure 1 — Control / Planning (run 35169868299, 8s)

Step `Set up Velnor workflow runtime` (`./.github/actions/setup-velnor-workflow`, `rev:
51e635af...`, generated line `ci-pr.yml:67`):

```text
##[error] no runtime product for revision 51e635aff74fdffab138d5751249ca7105b678c4
  (closure 1bfbdc50fa00fdfd); the mainline runtime-product publisher builds it after merge
```

- `51e635af` = "feat(a2): producer writes source revision; default-branch guard,
  verify-before-create", a commit on the PR branch (parent `33688938` = current main head).
  It is **not on main**.
- The setup action installs only attested release products (`velnor-workflow-runtime-v1-*`
  releases); it has no build-from-source fallback by design, and the error message states the
  only resolution: merge first, publisher builds after.
- Everything downstream (`plan` → all unit jobs → `Publish Velnor workflow runtime`) is
  `skipped` because unit jobs gate on `needs.plan.result == 'success'`.

## Failure 2 — Policy (run 35169868123, 28s) — downstream of Failure 1

Step `Acquire candidate generator product`:

```text
##[error] no same-repository PR run published candidate
  velnor-workflow-candidate-7dcabc83ea2c2a6b-Linux-X64
```

Causal chain (verified in logs):

1. Policy runs `pull_request_target`, i.e. the **base** `ci-policy.yml` (`BASE_PIN=7341ef4b`,
   the main pin — confirmed in the step env; the PR-head workflow embeds `51e635af`).
2. Audited-head pin `51e635af` resolves to a different closure than the base pin, so Policy
   takes the **candidate path**: it polls sibling `ci-pr.yml` runs at `HEAD_SHA=f4d46b3b` for
   artifact `velnor-workflow-candidate-<pin-candidate-closure16>-Linux-X64`.
3. That artifact is published only by the `github-rust-velnor-workflow` unit job
   (`candidate_publish: true`, `ci-pr.yml:976`), which `needs: [plan]` and was skipped when
   Planning failed. All sibling runs completed without publishing it → after one poll cycle
   Policy exits 1 (`seen=true`, `waiting=false`).

So Failure 2 is starvation caused by Failure 1, not an independent Policy/candidate-path bug.

## Ruled out

- **Producer-split logic broken? No.** The `runtime_products.rs` diff (default-branch guard in
  `closure` job, `--revision` self-report checks in build + publish, manifest gains
  `revision:`, assemble→verify-before-create reorder) is coherent and matches the PR title.
  None of it executes on this PR's CI path — the failure precedes any of it.
- **Candidate path / G4 ordering broken? No.** `ci-pr.yml` still publishes the candidate from
  the rust unit job exactly as on main (`candidate_publish: true` present at head and base);
  `ci-policy.yml`'s diff is **pin-only** (3 hunks, all `rev:`/`BASE_PIN:`/`POLICY_REVISION:`
  substitutions); the setup action is byte-unchanged (state hash `e091e9e1…` identical).
  Ordering (setup → plan → units → candidate publish; Policy polls sibling) is intact.
- **Pre-existing on main? No.** `CI / Main` @ `33688938` (run 35159519365) and
  `Runtime products` @ same SHA are both `success`. (Main's `Preview` workflow fails on
  `Guest payload x86_64/aarch64` — unrelated, pre-existing, different workflow.)

## Cross-check: the established two-phase flow

- Precedent `386ccc16` "chore(ci): bump D19 pin to f16cc51a **after gate-lanes merge**" touched
  **only** the pin + regenerated workflows — no generator source. Generator merges first,
  pin bump follows.
- All four sibling campaign branches (`feat/a2-consumer-negatives`, `-provision-promotion`,
  `-signer-order`, `-source-identity`) keep the old pin `7341ef4b`. Only #916 self-bumped.
- The `.github-gen/velnor-workflow.toml` comment ("bump it in a single pin commit after the
  last generator change") + the setup-action error text ("the mainline runtime-product
  publisher builds it after merge") both describe merge-then-bump. A same-PR self-bump to an
  unmerged rev **can never pass**: it is a chicken-and-egg by construction (setup needs the
  product that only a post-merge publisher run creates; the candidate that could substitute
  is published downstream of the setup that fails).

## Proposed fix (for the follow-up author; no edits made here)

Restructure #916 into a pure **phase-1 generator PR**:

1. On the #916 branch, revert `.github-gen/velnor-workflow.toml` pin to
   `7341ef4bdf750c1fbe419e94fb3848c5b8dde718` and revert **all** regenerated files
   (`.github/workflows/*`, `.github/ci/.github-actions-generator-state`) to base.
   Keep **only** `crates/velnor-workflow/src/primitives/runtime_products.rs`.
2. Push. Expected CI: Planning setup uses old pin (product exists) → plan succeeds →
   units run (the rust unit compiles + tests the new generator source and publishes a
   harmless candidate) → Policy takes the same-closure path, re-renders with the base
   runtime, tree is byte-identical → green. Merge.
3. Wait for the post-merge `Velnor workflow runtime products` run to publish the release
   for the merge commit's closure (it now runs the new producer with default-branch guard).
4. Follow-up **phase-2 PR**: bump pin to the merge commit + regenerate the tree. Planning
   setup then resolves an existing product, the candidate publishes, Policy verifies the
   regenerated tree with the candidate → green. Merge.

Do NOT attempt: keeping the self-bump (unfixable pre-merge — no product can exist), or
adding a build-from-source fallback to the setup action (would defeat the attested-product
trust model; the fail-closed behavior is the design working as intended).
