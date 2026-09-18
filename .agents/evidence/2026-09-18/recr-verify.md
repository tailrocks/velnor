# recovery-R verification: PR tailrocks/velnor#935 (recovery/runner-side @ ad06953d)

Authority: /tmp/evilmerge-forensic.md S1+S2. Scope: crates/velnor-{runner,control,model}/** +
crates/velnorctl/** + 4 docs mdx + schemas/velnor.telemetry.v1.json + 2 deletions.
Method: detached worktree /tmp/recr-wt @ ad06953d; object-level diffs vs origin/main@58a94b12
and source 8b7b4ac1; gates re-run locally; CI read via gh. No pushes/merges/amends.

## VERDICT: MERGE-OK

## 1. COMPLETENESS — PASS

- Scope LOST rows in forensic S1: exactly 100 (4 docs + 3 control + 3 model +
  86 runner + 3 velnorctl + 1 schema). Mechanical cross-check: all 100 present in
  `git diff --name-only 58a94b12..ad06953d`; zero scope rows missing.
- Per-file blob check `rev-parse <sha>:<path>` for all 100: **ALL 100 branch blobs
  byte-identical to 8b7b4ac1**. No dropped-with-reason rows needed in scope — nothing
  dropped. (Author's two deferrals in commit bodies — GUEST-PAYLOAD release.rs hunk,
  SEC-B1 workflow lib/release hunks — are velnor-workflow files, sibling PR-W's scope,
  correctly untouched here.)
- Deletions: `container/host_budget.rs` (D) and
  `velnorctl/tests/job_resource_flags_single_source.rs` (D) both absent on branch
  (cat-file 128); `git diff --name-status` shows exactly these 2 D rows, no renames.
- Scope containment: PR diff = 103 files = 100 LOST + 2 deletions + exactly one extra:
  `.github/ci/.github-actions-generator-state` (allowed generated/state). Nothing else
  outside scope touched. `git diff 8b7b4ac1..ad06953d` shows 105 files, ALL out of
  scope (workflow-side + generated + Dockerfile + plans ledger) — zero in-scope
  deviation from source.
- Identifier sanity: `mod permit_ledger`, `mod scaleset`, ScaleSetClient (11 files),
  DaemonWorkerLane (4), AdmissionRejection (2) present; zero `host_budget` refs
  outside kept plans dir.

## 2. FAITHFULNESS — PASS

- Byte-identical blobs to source for every re-ported file ⇒ no redesign possible, no
  adapted hunks to judge, no behavior change. (Adaptation was unnecessary: forensic
  proves tip==main==base for every in-scope file, so pure blob restore is correct.)
- No s1/s2 duplication: branch-vs-main diff touches zero velnor-workflow files.

## 3. PIN+BASE+DCO — PASS

- Pin: all 82 `ec399527…` lines identical main-vs-branch (same files, same counts;
  only the grep ref-label prefix differs). Unchanged.
- Base: merge-base(branch, origin/main) == 58a94b12 == origin/main HEAD; re-fetched
  immediately before verdict — main has NOT moved (PR-W has not merged). Not stale.
- DCO: Signed-off-by (Alexey Zhokhov) present on all 9 commits. All 9 subjects are
  Conventional (`feat/fix/chore(runner|ci)`).

## 4. REGEN — PASS

Built `velnor-workflow` in worktree; ran `velnor-workflow . --plain --force` → exit 0,
`Generated 21 files`, `git status` empty (clean). Then `--plain --dry-run` → 21/21
`unchanged`, exit 0, zero diff. Regen churn = only the committed 1-line scan-fingerprint
move in generator-state (caused by the recovered sources themselves); no generated
workflow file changed. Matches author's ad06953d message (20/21 + fingerprint line;
observed 21/21 unchanged post-commit).

## 5. GATES (re-run in /tmp/recr-wt) — PASS

CI runs units as `mbx nextest run --locked --all-features` (project.toml); the
scaleset suites are `#![cfg(feature = "test-support")]`, so `--all-features` is the
correct local invocation (bare `cargo test` yields 0 tests per suite — expected, not a gap).
- velnor-runner (nextest, all features): **2488 passed, 5 skipped, 0 failed**.
  Scaleset suites: allocator 6, daemon 11, loop 9, protocol 8, worker 3 = 37/37 pass.
  (daemon 11/11 matches forensic merge-04607e6b count.)
- velnor-workflow (nextest, all features): 1709 passed, 0 failed.
- velnor-workflow-contract: 6 passed.
- clippy `--all-targets -p velnor-runner -- -D warnings`: clean; same for
  -p velnor-workflow: clean. CI-parity form (`--locked --profile test --all-targets
  --all-features -p velnor-runner`): clean.
- `cargo fmt --check`: clean. `actionlint`: clean, exit 0.

## 6. CI — PASS (terminal, no wait needed)

Run 35253271364 on head ad06953d, all 76 checks terminal (no pending):
- Policy: SUCCESS (23s). DCO: pass.
- Every executed /GitHub job SUCCESS (20 pass), incl. `velnor-runner / GitHub·hosted`
  (8m44s): CI log shows **2492 passed (1 leaky), 5 skipped**, all 5
  `velnor-runner::scaleset_{allocator,daemon,loop,protocol,worker}` binaries present.
  velnor-workflow, velnorctl, control, model, contract GitHub jobs all pass.
- Fail set (7): bun-velnor/velnor, docker/velnor, docs/velnor, opentofu/velnor,
  prepare-cargo, Required, ci-required — each failing in 2–3s at step
  `Velnor rejected job (operational_store)`: "operational store rejected the
  sanitized admission row; job failed closed before execution". Pure backend/
  environmental, pre-execution.
- Baseline: main@58a94b12 run 35242831003 fails on EXACTLY the same 7 jobs with the
  identical failing step. **Zero new failures vs main.**

## Notes / residual

- The 7 environmental velnor-lane failures block `ci-required`/`Required` on both main
  and this PR; merging with failing-required-checks needs the same override path main
  itself would need — flagging, not blocking this verdict (identical-to-main fail set).
- Worktree /tmp/recr-wt left in place (detached @ ad06953d) for audit; main repo untouched.
