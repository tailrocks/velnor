# PR 932 final verification: fix(s2) selection CSV @ 22cc123c (regen repair)

## VERDICT: MERGE-OK

The repair is exactly the prescribed 1-line state regen, regen is clean,
Policy is green via the candidate path, and every executed GitHub-lane job
passes. The only failures are the pre-existing Velnor `operational_store`
admission rejection (pre-execution, zero PR code run) plus its two
downstream required gates — the #931 merge-bar shape, wider only in fan-out
because the correct regen selects all units. Nothing was pushed, merged,
or amended.

- Heads (fresh fetch): branch `22cc123c4548c4e0d13392557ed94bbb8cb4b630`
  (99875eee + repair); `origin/main` = `52739e176c1cba3ddeeff1ca21eda2edadb52b17`,
  unmoved; `merge-base` = same. No HOLD-stale.
- Worktree: `/tmp/932-final-wt` (22cc123c, detached). Read-only except /tmp.
- `gh pr view 932`: `mergeable: MERGEABLE`, head = 22cc123c, DCO `SUCCESS`.
- Prior `/tmp/932-verify.md` content stands; this file covers only repair + CI.

## 1. REPAIR — PASS

- `git show 22cc123c --stat`: touches ONLY
  `.github/ci/.github-actions-generator-state`, 1 line:
  `scan e89ef785c3af99ce` → `a215cd4110969456` — exactly the digest the
  prior verification prescribed and CI expected. No source changes.
- DCO: `Signed-off-by: Alexey Zhokhov <alexey@zhokhov.com>` on the commit;
  PR `DCO` check `SUCCESS`.
- Base: `merge-base(branch, main)` = `origin/main` = PR `baseRefOid` =
  `52739e17` after fresh `git fetch`. Main unmoved, branch head unchanged.
- Pin unchanged: `.github/workflows` diff vs `origin/main` = **0 lines**;
  `.github/ci/project.toml` diff = 0; pin revision `a6fa8d4a5096e6df…`
  present in `ci-policy.yml`. Full PR diff = state + 3 src + 1 test file.

## 2. REGEN — PASS (fresh worktree @22cc123c)

- `cargo build -p velnor-workflow --bin velnor-workflow`: success.
- `./velnor-workflow --plain --force /tmp/932-final-wt`: exit 0,
  `git status --porcelain` EMPTY (byte-identical regen).
- `./velnor-workflow --plain --dry-run`: `Dry-run: 0 files would change`.
- Rendered `.github/workflows` diff vs `origin/main`: EMPTY (0 lines).

## 3. CI — PASS (terminal, one 4-min poll)

Runs at head (both `completed`): CI/PR `35224947867`, Policy `35224945016`
(`success` — the prior §5 cascade is resolved via the candidate path).

- **Policy: SUCCESS.** **velnor-workflow/GitHub: SUCCESS** (the `--check`
  gate passes with the regen committed).
- **Every executed /GitHub: SUCCESS** — 18/18 (bun, docker, docs, opentofu,
  all 13 rust units, Planning), zero GitHub-lane failures. Rest skipped.
- Fail set (7), all verified in job logs, all environmental:
  - 5× `Velnor rejected job (operational_store)` **before execution**
    (`no declared workflow command was executed`): Bun/Velnor, Docker/Velnor,
    Docs/Velnor, OpenTofu/Velnor, prepare-cargo — identical signature to the
    pre-existing #931 failures (job log `phase: operational_store`).
  - 2× downstream gates: `ci-required`
    (`selected CI job velnor-bun-velnor did not pass: failure`), `Control /
    Required` (exit 1).

### Baseline comparison (no new *cause* of failure)

Literal main@52739e17 CI/Main run `35220302468` is degenerate (unit legs
skipped), so leg-for-leg comparison is meaningless there; the informative
baseline remains merged #931's run `35219190605`
(docker/Velnor + prepare-cargo + ci-required + Required, same signatures).

Honest delta: bun/docs/opentofu Velnor legs **executed** in the new run
while skipped in the 99875eee and #931 runs. Mechanism is proven, not
guessed — Planning logs: old run `scope=affected units=docker,<5 rust>`
(6 units); new run `scope=affected units=bun-velnor,docker,docs,opentofu,
<13 rust>` (all 17). The committed scan-digest change marks generator
inputs changed, so affected-unit detection conservatively fans out to all
units. Those 3 legs then hit the *identical pre-existing environmental
admission rejection* with zero PR code executed — the same condition that
fails docker/Velnor + prepare-cargo on every run including merged #931.

This is not a PR-caused failure and has no author-side remedy: the only
fix is a Velnor deployment allowlist change, out of PR scope; demanding
narrower fan-out would mean un-committing the required regen (contradicts
the prior HOLD). Shape matches the predicted post-fix CI (#931-shaped:
Policy + GitHub legs green, Velnor-lane + required-gate reds
environmental), wider only in correct fan-out.

## Per-item scorecard

1. REPAIR — PASS (state-only 1-line digest, DCO, fresh base, pin intact).
2. REGEN — PASS (force clean, dry-run 0, workflows diff empty).
3. CI — PASS (Policy + velnor-workflow/GitHub + all GitHub legs SUCCESS;
   fail set within environmental scope; no new failure cause vs baseline).

**VERDICT: MERGE-OK** — merge `fix/s2-selection-csv` @ `22cc123c` into
`52739e17`. (Not merged by verifier per instructions.)
