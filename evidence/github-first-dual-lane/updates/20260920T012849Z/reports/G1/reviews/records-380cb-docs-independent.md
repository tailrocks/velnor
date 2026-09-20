# Independent records-docs rereview — `380cb85e511d9d2bbfa90626290eb53bb835ff1a`

## Verdict

**REJECT source-doc approval at this exact revision.** The correction fixes the
prior stale scan, bootstrap, APT, and PR963 disposition references. One
revision-identity error remains: it binds current PR963 head `c440d4db` to
base `d20d4d1`, but the live PR API reports base `1048337062`. Do not treat
the current PR as tested against `d20d4d1`, and do not transfer any review
across this base/head mismatch.

This is a docs-only verdict, not merge or gate authority.

## Exact checkout and checks

- Exact HEAD: `380cb85e511d9d2bbfa90626290eb53bb835ff1a`
- Parent: `a386797d86ebfcb10ce403dcb5e59891a010ca17`
- Detached worktree: `/tmp/velnor-records-380cb-review`; clean.
- Remote records branch: same `380cb85e`; read-only current `main` query:
  `e94b48406c4ed206fce2bbf39b788264e72cf39c`.
- `rtk git diff --check HEAD^ HEAD`: **pass**.
- `rtk proxy npx --no-install markdownlint-cli2@0.20.0 docs/ci/github-first-dual-lane/{PLAN,STATUS,RUNBOOK}.md`: **0 errors**.
- Goal/fleet scope: exact 32-entry set, no diff.

## Blocking finding

### RCD-5 — current PR963 base is recorded incorrectly

The new current-query rows in `PLAN.md:136`, `STATUS.md:118-120`, and
`RUNBOOK.md:118-123` state:

```text
PR963 head c440d4db... base d20d4d1...
```

The read-only GitHub API query at review returned PR963:

```json
{"number":963,"state":"open",
 "base":"1048337062ea625fada1b4f7c07f2feed75f60c7",
 "head":"c440d4db3fd59a9e4abd396d7a75e670c4f3d862",
 "updated_at":"2026-09-20T00:45:53Z"}
```

`d20d4d1d...` is the older current-main observation used by the 0c1 review
and the v4 authority checkpoint; it is not PR963's declared base. Current
remote main has since advanced to `e94b48406c4ed206fce2bbf39b788264e72cf39c`.
Correct the rows to the exact API tuple (`head c440d4db`, `base 104833706`),
and separately label d20/e94 as time-bound main observations. The 0c1 source
approval does not transfer to c440d4db, and no current-head approval exists in
this review.

## Rechecked prior corrections

- `PLAN.md:339`, `STATUS.md:210,252`, and the scan status now identify
  `6409a086`/`7e9a2b5f`/`3c46b9e`/`0a15fd06` as rejected or blocked, matching
  the exact scan reports.
- `b981f43e` is now explicitly rejected in PLAN/STATUS/RUNBOOK with its exact
  independent report and owner-test boundary.
- APT `91bdf6c` is now explicitly blocked with its exact independent review;
  no publication/G2 admission is implied.
- Historical `fb78d85` is labeled historical; current `c440d4db` is explicitly
  unreviewed, so no old source approval transfers.
- Goal order remains G0 → G1 → G2 → G3 → G4/G5 → G6 → G7; exact 32 scope,
  no-false-gate language, and prohibition of aliases/legacy paths remain sound.

## Scope

Read-only detached review. No source, branch, GitHub, generated output, or
implementation files were modified.
