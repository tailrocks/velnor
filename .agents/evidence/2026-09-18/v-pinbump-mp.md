# VERDICT: CERTIFIED — pin-bump-M_p PR #922 safe to merge normally

PR: https://github.com/tailrocks/velnor/pull/922
Head: `8cd5de6659aadacd81192bd422c5b02f00c409f3` (single commit, OPEN, MERGEABLE, unchanged throughout review)
Base: `main @ a6fa8d4a5096e6df37250b59a8105abfebdb9013` (= M_p, #916 merge) — EXACT match at PR open.
DCO: `Signed-off-by: Alexey Zhokhov <alexey@zhokhov.com>` present; DCO check PASS.

## DRIFT (recorded, does not block)
origin/main moved DURING review: `a6fa8d4a` → `b6f53c09` (external PR #921 merged:
1 source file `crates/.../primitives/check_profiles.rs`, ZERO tree files).
Merge simulated in scratch (`/tmp/v-pinbump-mp-merge`): applies CLEANLY, zero conflicts
(merge-tree: 0 markers; GitHub mergeable=MERGEABLE, mergeState=BLOCKED solely on
precedent-identical ci-required noise).
Post-merge main-green PROVEN: pin renderer (generator@a6fa8d4a) `--plain --check` on the
merged tree exits 0 (merged tree byte-identical to pin render → Policy generated-tree
will PASS on main). Merged-tree closure `72c3446a…` ≠ pin closure `10a71b77…` (#921
generator delta) → pin will be closure-stale after merge; CI provisions
VELNOR_WORKFLOW_PINNED_BINARY for unit `--check` (ci-unit-rust.yml:240,806, closure-
verified), so lanes stay green. A follow-up pin-bump to the new merge commit restores
full pin currency — shepherd/campaign next step, not a defect in this PR.

## 1. Diff = PIN+REGEN ONLY (independently proven, not trusted)
- 13 files, ALL under `.github-gen/` (pin) or `.github/` (state + 11 workflow yamls); zero source changes.
- 94 `-` / 94 `+` lines; minus/plus sets IDENTICAL after normalizing the two pin SHAs
  (`12b25700…` → `a6fa8d4a…`) and 16-hex state digests — every changed line is a pin
  substitution (`rev:`, `EXPECTED_REVISION:`, `VELNOR_WORKFLOW_POLICY_REVISION`,
  `PINNED_REVISION:`, `BASE_PIN:`, artifact names) or a state digest refresh.
- `revision = "a6fa8d4a5096e6df37250b59a8105abfebdb9013"` in `.github-gen/velnor-workflow.toml`.
- Zero old-rev (`12b25700…`) remnants in the PR tree.

## 2. Gates in scratch worktree (fresh `/tmp/v-pinbump-mp-wt` @ 8cd5de66, tree clean)
Built generator from worktree source (`cargo build --locked`, ~11s):
- `closure(8cd5de66) == closure(a6fa8d4a) == 10a71b77b0967566…` (pin worktree
  `/tmp/v-pinbump-mp-pin` binary cross-check) — generator source identical to pin ✓
- `--plain --dry-run`: "0 files would change", exit 0 ✓
- `--plain --check`: "Generated files are current", exit 0 ✓
- `cargo test --locked -p velnor-workflow`: 868 passed / 0 failed, exit 0 ✓
- clippy `--profile test --all-targets -p velnor-workflow -D warnings`: 0 warnings ✓
- `cargo fmt --check -p velnor-workflow`: clean ✓; actionlint: no findings ✓

## 3. PR CI — fully green per campaign definition (settled: 20 pass / 42 skip / 7 fail, 0 pending)
PASS: DCO, Policy, Control/Planning, ALL 17 GitHub unit lanes (Bun, Docker, Docs,
OpenTofu, dep-policy, topology, unit-collector, bench, client, control, model, render,
runner, tools, workflow, workflow-contract, velnorctl). Non-GitHub lanes SKIP = Planning
selection, not failures.
- Policy via CANDIDATE path PROVEN (run 35195944909 job 105119078281, success):
  `BASE_PIN=12b25700…` (old) vs declared pin `a6fa8d4a…`;
  `VELNOR_WORKFLOW_PINNED_BINARY=…/velnor-workflow-candidate/velnor-workflow` acquired;
  Enforce: 11 rules, 0 failed, incl. `generated-tree … byte-identical to the render
  of velnor-workflow at a6fa8d4a…`.
- FAIL set (7): Bun/Velnor, Docker/Velnor, Documentation/Velnor, OpenTofu/Velnor,
  Control/Prepare-Cargo/prepare-cargo, Control/Required, ci-required — check-NAME-
  identical to #920's settled run AND #917's merged run. Bun/Velnor + prepare-cargo
  failure text VERBATIM identical to precedent (`Velnor rejected job
  (operational_store)` / `operational store rejected the sanitized admission row;
  job failed closed before execution`). ci-required/Required fail ONLY downstream of
  that admission noise. Infra rejection, out of scope for a pin-bump.

## 4. Merge mechanics (reported, shepherd owns the merge)
- Ruleset requires DCO ✓, Policy ✓, ci-required ✗(noise-only, precedent-identical).
- Precedent: #917 and #920 merged to main with the IDENTICAL 7-red set. No new
  authorization needed: Policy+DCO green, noise pre-existing.
- Post-#921-drift: merge is conflict-free and main-green is proven above (pinned-
  renderer check on simulated merge). Recommend shepherd merge promptly, then queue
  the follow-up currency bump.
- No edits made by reviewer; no merge performed.

## Verdict rationale
Base exact at open, drift recorded + neutralized ✓ · PIN+REGEN-only proven ✓ ·
scratch gates green ✓ · Policy green via candidate path ✓ · Planning + DCO + all
GitHub lanes green ✓ · red set name- and text-identical to merged precedent ✓ ·
head unchanged, merge conflict-free, merged-tree main-green proven ✓.
CERTIFIED.
