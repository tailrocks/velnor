# Independent records-docs rereview — `89e1fe0ac22cc6ceaa2f294bcb38e6065c16afd4`

## Verdict

**APPROVE source-doc correction at this exact revision.** The PR963 API tuple
is now exact, current `main` is explicitly separate and under fresh census,
and d20 is historical. Prior rejected/blocked checkpoint dispositions remain
revision-bound; no source approval transfers to PR963 `c440d4db`, and no gate
or merge approval follows.

## Exact checkout and checks

- Exact HEAD: `89e1fe0ac22cc6ceaa2f294bcb38e6065c16afd4`
- Parent: `380cb85e511d9d2bbfa90626290eb53bb835ff1a`
- Detached worktree: `/tmp/velnor-records-89e1-review`; clean.
- Remote records branch: same `89e1fe0a`.
- Read-only remote `main`: `e94b48406c4ed206fce2bbf39b788264e72cf39c`.
- Current PR963 API tuple: head
  `c440d4db3fd59a9e4abd396d7a75e670c4f3d862`, base
  `1048337062ea625fada1b4f7c07f2feed75f60c7`; no exact-head approval.
- `rtk git diff --check HEAD^ HEAD`: **pass**.
- `rtk proxy npx --no-install markdownlint-cli2@0.20.0 docs/ci/github-first-dual-lane/{PLAN,STATUS,RUNBOOK}.md`: **0 errors**.

## Review result

- `PLAN.md:135`, `STATUS.md:116-121`, and `RUNBOOK.md:120-124` now bind
  PR963 to API base `1048337062`, retain `c440d4db` as unreviewed, and state
  that `0c1ec757` approval does not transfer.
- `PLAN.md:135`, `STATUS.md:61-67,120-122`, and `RUNBOOK.md:124` distinguish
  later remote main `e94b484` as a separate fresh census and d20 as historical
  v4-capture evidence. The older b5/fb78 rows remain explicitly historical.
- Earlier corrections remain present: scan candidates through `0a15fd06` are
  rejected/blocked; bootstrap `b981f43e` is rejected; APT `91bdf6c` is
  blocked; owner test counts remain non-gate evidence.
- Gate order, exact 32-repository scope, no-false-gate boundaries, and
  prohibition of aliases/legacy paths remain intact.

## Scope

Read-only detached review. No source, branch, GitHub, generated output, or
implementation files were modified.
