# Independent records-docs review — `a386797d86ebfcb10ce403dcb5e59891a010ca17`

## Verdict

**REJECT source-doc approval at this exact revision.** The gate graph, fixed
32-repository scope, fail-closed language, and no-alias rules are sound. The
current task/checkpoint tables are not revision-accurate: they point at a
rejected scan candidate and call completed blocking reviews pending. Do not
integrate this records revision as the current plan until those references are
corrected. This is a docs verdict only; it is not merge or gate authority.

## Exact checkout and checks

- Branch: `codex/github-first-records`
- Exact HEAD: `a386797d86ebfcb10ce403dcb5e59891a010ca17`
- Parent: `071a1d7ae73bb2ac5b2d8c3e03b9598a1a526e7d`
- Remote branch at review: same `a386797d`; remote `main`:
  `d20d4d1d17590cca85b501d982cbaad70d42c641`.
- Detached review worktree: `/tmp/velnor-records-a386-review`; clean.
- `rtk git diff --check HEAD^ HEAD`: **pass**.
- `rtk proxy npx --no-install markdownlint-cli2@0.20.0 docs/ci/github-first-dual-lane/{PLAN,STATUS,RUNBOOK}.md`: **0 errors**.
- Goal list and `fleet.json`: **32 entries, exact set** (`diff -u` pass;
  `jq` uniqueness/count pass).

## Blocking findings

### RCD-1 — current scan task points at a rejected candidate

`PLAN.md:339`, `STATUS.md:210`, and `STATUS.md:252` identify `6409a086` as
the scan-integrity candidate with approval pending. The external exact report
`G1/scan-integrity/REPORT.md` names corrected candidate `3c46b9e83c9a0ca57be88743e49ecae27f731685` and explicitly says not to integrate
`6409a086` or `7e9a2b5f`; `G1/scan-integrity/corrected-independent-review.md`
rejects `3c46b9e` too (for forged sidecar ownership and `./.github` source
bypass). `STATUS.md:222` already records scan `3c46b9e` as rejected/blocked,
so the task table contradicts the status table. Replace the stale candidate
with the exact corrected/rejected SHA and its blocker, or with the next
unreviewed candidate; never leave `6409a086` as an actionable review target.

### RCD-2 — bootstrap review outcome is falsely still pending

`PLAN.md:190-195`, `PLAN.md:326`, `RUNBOOK.md:64`, `STATUS.md:53`, and
`STATUS.md:243` describe the `b981f43e8dfd70b4c628d29b0e7e9dce679ce537`
security/transport review as pending. External
`G1/reviews/bootstrap-b981f43e-independent.md` is revision-bound to that exact
SHA and has verdict **Reject as a G1 source checkpoint**, with P1 gaps in
independent source archive/API-tree proof, attempt/freshness binding,
action/upload provenance, schema-1 legacy paths, transport fixtures, and
empty runtime image digests. Update all three docs to `changes required`/
`rejected`, link the exact report, and retain owner 1,727-test output as
owner-reported only. A pending label hides an actual blocking review.

### RCD-3 — APT review outcome is falsely still pending

`PLAN.md:197`, `STATUS.md:54`, and `RUNBOOK.md:65` say exact APT `91bdf6c`
review is pending. External
`G1/reviews/apt-83e7ab4-91bdf6c-independent.md` reviews
`91bdf6cc1d0a5c429c5c01f17bf15dbb153c661b` and verdicts **BLOCKED**, with
provider/source authority, verify-to-publish descriptor binding, extraction
confinement races, absent native producer handoff, and generated actionlint
failures. Mark it blocked/changes-required and preserve the no-publication
boundary. Do not report only “independent review pending.”

### RCD-4 — old PR963 hash is historical, while current heads are absent

The PR963 rows in `PLAN.md:118-134`, `STATUS.md:89-117`, and
`RUNBOOK.md:91-120` are explicitly timestamped `b5`-bound observations, so
they do not create a false gate by themselves. However they call `fb78d85`
the signed replacement while the external source review records approval only
for `0c1ec757`, and the read-only current PR query now reports head
`c440d4db3fd59a9e4abd396d7a75e670c4f3d862` (base
`d20d4d1d17590cca85b501d982cbaad70d42c641`). Keep `fb78d85` clearly
historical, add the current unreviewed head/its fresh review state, and do not
transfer the `0c1ec757` source approval to `c440d4db`.

## Passing design checks

- Goal order is preserved: G0 → hosted G1 → package G2 → hosted-fleet G3 →
  actual-host G4/G5 → dual-provider G6 → independent G7. The 32-row goal
  manifest exactly matches `fleet.json`; early `g3-*` rows are explicitly
  read-only and operationally depend on G2.
- `PLAN.md`, `STATUS.md`, and `RUNBOOK.md` consistently say no gate has
  passed and distinguish source/checkpoint evidence from hosted/runtime proof,
  except for the stale dispositions above.
- Alias/legacy language is prohibitive: canonical typed records are required;
  CLI/serde aliases, fallback paths, dual parsers, and precedence shims are
  rejected. No active compatibility alias is introduced by this docs commit.

## Scope

Read-only detached review. No source files, branches, GitHub state, generated
outputs, or implementation files were modified.
