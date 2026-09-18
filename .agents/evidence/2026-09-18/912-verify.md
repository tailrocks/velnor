# PR tailrocks/velnor#912 verification — "docs(plan): bastion final plan"

- Branch: `docs/bastion-final-plan` @ `b3ff4626` (= `8b7b4ac1` + merge `d3895648` of main `231253b3` + regen)
- Head SHA: `b3ff4626c238c0768afb7598cbef16ae2efb9467` (matches PR headRefOid)
- Verified: 2026-09-17 ~15:50 UTC. Read-only on repo (worktree in /tmp, removed after). No push/merge/amend.

## 1. PLANS INTACT — PASS

All bastion files byte-identical (sha256) between `8b7b4ac1` and HEAD; `git diff 8b7b4ac1..HEAD -- plans/bastion-three-provider-ci/` empty:

| file | sha256 |
|---|---|
| spec.md | 443ba65c…f0824ef IDENTICAL |
| work-plan.md | 6befd12e…64c498 IDENTICAL |
| checklist.md | 4a6e98c1…4622a1e24 IDENTICAL |
| evidence.md | c3c562a3…58a3b20c6 IDENTICAL |
| README.md | df3d5066…76522e5dd IDENTICAL |
| goal.md | e6956b65…4be97b74db IDENTICAL |

`plans/` DOES exist on main@231253b3 (9 files: 6 dated memos + docker-multi-arch + slice-a + slice-c).
Reconcile: `git diff 231253b3..HEAD -- plans/` shows ONLY the 6 bastion additions (+1347 lines);
all 9 main files untouched. No conflict, purely additive.

## 2. TREE == MAIN + PLANS — PASS (one documented mechanical exception)

`git diff 231253b3..HEAD --name-status` (7 files):
- A × 6: `plans/bastion-three-provider-ci/{README,checklist,evidence,goal,spec,work-plan}.md`
- M × 1: `.github/ci/.github-actions-generator-state` — ONE line: `scan c444cafbb73df773 → 1eb8da53ab3d7d95`

Attribution:
- Merge `d3895648` vs main (`231253b3..d3895648`): ONLY the 6 plans/ files → merge took main's side everywhere else ✓
- Regen `b3ff4626` (`d3895648..b3ff4626`): ONLY the scan-fingerprint line.

Zero source/workflow/pin deltas: `git diff 231253b3..HEAD -- .github-gen/ .github/workflows/` is EMPTY;
in the state file, `config` hash, `generator 50`, and every `[outputs]` hash are unchanged, so no
generated workflow bytes differ. The fingerprint bump is the expected, Policy-required regen artifact
(delete it and D19 byte-identity fails). Non-plans/ file count: 1, mechanical, expected.

## 3. PIN+BASE — PASS

- Pin: `.github-gen/velnor-workflow.toml` → `revision = "ec3995277f82473777f18969e58ea76f63e54cfd"` ✓ (the R2m flip; byte-identical to main, zero delta; workflows carry the same rev)
- `231253b3` is an ancestor of HEAD (`merge-base --is-ancestor` → YES)
- DCO: `Signed-off-by: Alexey Zhokhov <alexey@zhokhov.com>` present on BOTH `d3895648` (merge) and `b3ff4626` (regen)

## 4. REGEN — PASS (fresh worktree /tmp/912-verify-wt @b3ff4626, since removed)

- `cargo build --locked --offline` in `crates/velnor-workflow` → exit 0; binary `--revision` = `b3ff4626…` (built from HEAD)
- `./velnor-workflow --plain --force .` → "Generated 21 files", exit 0; `git status` after → CLEAN ✓
- `./velnor-workflow --plain --dry-run .` → "Dry-run: 0 files would change", exit 0 ✓

## 5. CI — PASS (settled 20 pass / 7 fail / 49 skipping / 0 pending; waited ~12 min)

- **Policy: SUCCESS** (21s, run 35241222100) ✓
- Every executed GitHub lane SUCCESS: 17 × `GitHub · hosted` checks pass; zero executed GitHub-lane non-pass ✓
- Fail set (7), ALL within environmental scope:
  - 5 × velnor admission, all 3s, signature: "Velnor rejected this job before workflow execution / phase: operational_store / reason: operational store rejected the sanitized admission row; job failed closed before execution / effect: no declared workflow command was executed" (verified in bun-velnor + prepare-cargo logs; docker-trusted, docs, opentofu same 3s shape): `bun-velnor/velnor`, `prepare-cargo`, `docker/velnor·trusted`, `docs/velnor`, `opentofu/velnor`
  - 2 × rollups: `Control / Required`, `ci-required` ("expected CI job velnor-bun-velnor did not pass: failure") — pure consequences of the 5 admission fails
- Main baseline @231253b3: SAME shape — identical 5 velnor fails + identical 2 rollup fails + Policy success; plus 2 main-only `Guest payload` (aarch64/x86_64, release lane, absent on PR). PR introduces zero new failures.
- PR state: OPEN, MERGEABLE, no conflicts. mergeStateStatus=BLOCKED reflects only the environmental fails (main itself is red the same way).

## VERDICT: MERGE-OK

All five checks pass. Branch = main + 6 intact plan docs + 1 required regen fingerprint line; pin correct; DCO present; regen idempotent (dry-run 0); Policy green; every GitHub lane green; fail set exactly the environmental velnor-admission + rollup set already present on main. Not merged per instructions.
