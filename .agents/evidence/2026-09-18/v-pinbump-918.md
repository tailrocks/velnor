# VERDICT: CERTIFIED — pin-bump-918 PR #920 safe to merge normally

PR: https://github.com/tailrocks/velnor/pull/920
Head: `7c434c8da8d5de70c58ceac452f889783637dea7` (single commit, OPEN, MERGEABLE, no drift during review)
Base: `main @ 033ab54675c5ec9386f5da3f7fdf2b4bc7217a3c` — EXACT match, origin/main unmoved (no drift)
DCO: `Signed-off-by: Alexey Zhokhov <alexey@zhokhov.com>` present; DCO check PASS

## 1. Diff = PIN+REGEN ONLY (independently proven, not trusted)
- 13 files, ALL under `.github-gen/` or `.github/`; zero source changes.
- 94 `-` / 94 `+` lines; minus/plus sets IDENTICAL after normalizing the two pin SHAs
  (`08ea1b07…` → `033ab546…`) and 16-hex state digests — every changed line is either a
  pin substitution or a `.github-actions-generator-state` digest refresh (config + 12 outputs).
- `revision = "033ab54675c5ec9386f5da3f7fdf2b4bc7217a3c"` in `.github-gen/velnor-workflow.toml`.

## 2. Gates in scratch worktree (fresh `/tmp/v-pinbump-918-wt` @ 7c434c8d, tree clean)
Built generator from worktree source (`cargo build --locked`, 11.8s):
- `closure(7c434c8d) == closure(033ab546) == 71e623219d2db42c…` — generator source identical to pin ✓
- `--plain --dry-run`: "0 files would change", exit 0 ✓
- `--plain --check`: "Generated files are current", exit 0 ✓
- `cargo test --locked -p velnor-workflow`: 860 passed / 0 failed / 16 suites, exit 0 ✓
- clippy `--all-targets`: 0 warnings/errors; `cargo fmt --check`: clean; actionlint: no findings ✓

## 3. PR CI — fully green per campaign definition (settled: 16 pass / 46 skip / 7 fail, 0 pending)
PASS: DCO, Policy (3m20s), Control/Planning (21s), all 13 Rust …/GitHub lanes
(topology, dependency-policy, unit-collector, 10 crates). Bun/Docker/Docs/OpenTofu GitHub
lanes SKIP = Planning selection (pin-only diff touches no unit inputs), not failures.
- Policy via CANDIDATE path PROVEN (log job 105100904026): runs on base-owned
  `pull_request_target` with `BASE_PIN=08ea1b07…` (old) vs declared pin `033ab546…`;
  `VELNOR_WORKFLOW_PINNED_BINARY=…/velnor-workflow-candidate/velnor-workflow` acquired;
  Enforce PASS on all 9 rules incl. `generated-tree … byte-identical to the render of
  velnor-workflow at 033ab546…`. Rendezvous fix working as designed.
- FAIL set (7): Bun/Velnor, Docker/Velnor, Documentation/Velnor, OpenTofu/Velnor,
  Control/Prepare-Cargo/prepare-cargo, Control/Required, ci-required — check-NAME-identical
  to #917's merged run 35184057652 AND #918's run; Bun/Velnor failure text VERBATIM identical
  to #917 (`Velnor rejected job (operational_store)` / `operational store rejected the
  sanitized admission row; job failed closed before execution`). Same jobs, same mode.
  ci-required/Required fail ONLY downstream of that admission noise. Shepherd's re-run
  reproduced the same set deterministically (~3s) — infra rejection, not flake, out of
  scope for a pin-bump (needs a Velnor capability publish).

## 4. Merge mechanics (reported, shepherd owns step 5)
- Ruleset `protect-main` (active): requires DCO ✓, Policy ✓, ci-required ✗(noise-only);
  0 approvals required; merge/squash/rebase allowed. `mergeState=BLOCKED` is SOLELY the
  precedent-identical ci-required red.
- Precedent: #917 merged to main with the IDENTICAL 7-red set (08ea1b07 on main, its
  ci-required check red). No new authorization needed: Policy+DCO green, noise pre-existing.
- No edits made by reviewer; no merge performed.

## Verdict rationale
Base exact ✓ · PIN+REGEN-only proven ✓ · scratch gates green ✓ · Policy green via
candidate path ✓ · Planning + DCO + all running GitHub lanes green ✓ · red set
byte-identical to merged precedent in both names and failure text ✓ · no drift ✓.
CERTIFIED.
