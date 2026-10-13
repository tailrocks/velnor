# Production-readiness reconciliation index

Index snapshot captured: `2026-08-26T22:51:21Z`
Admission refresh indexed: `2026-08-27` (exact UTC time unavailable)

Provenance: the index snapshot time above is exact UTC. The admission refresh
date is known, but its exact UTC capture time is unavailable and is not
inferred. Current PR head attribution: `tailrocks/velnor`,
`7489b6b07edfa75e589a2a35f108ffe3bd24e7f9`.

API scope: GitHub API inspection of the listed repositories, workflow runs,
jobs, and check suites, plus ChainArgos organization runner-group policy,
repository-selection, and runner-list endpoints for runner group `4`. The
2026-08-27 exact-target remediation attempt below targeted only the three
listed ChainArgos run/suite pairs. No other runs, suites, runners, files,
workflows, or policies changed. Prior cleanup-attempt evidence records earlier
cancellation requests and the removal of eight stale validation-owned
registrations; those historical actions are not actions of this remediation
attempt and do not establish current state.

This is a sanitized reconciliation of the supplied production-readiness evidence
plus exact-target remediation evidence; it is not wholly read-only. It does not
establish production readiness or complete any plan gate.
The older P0 inventories and cleanup attempt referenced by the supplied
evidence are historical snapshots; they are not current state.

## Dated read-only recheck — 2026-08-27

Exact capture time unavailable. The recheck recorded the following terminal
run state: `33010150644`, `32987670118`, `32962658148`, and `32940470688` are
completed with cancelled conclusions. They are no longer unresolved queue
objects.

Run `33012336003` and check suite `89435047597` returned API HTTP 404. They
remain unresolved absent GitHub Support confirmation that the objects have
been removed or otherwise reached an authoritative terminal state.

ChainArgos runs `32985134450`, `32984965998`, and `32984867843`, with check
suites `89353010038`, `89352428140`, and `89352110318` respectively, remain
queued with null/zero check-run state unchanged since
`2026-08-26T15:22–15:26Z`. No fresh Sentry probe was performed; prior runner
registration state remains historical only.

## Exact-target remediation attempt — 2026-08-27

Capture date: `2026-08-27`; exact time unavailable. Only the following
ChainArgos run/suite pairs were targeted. For each pair, the initial run and
suite reads returned HTTP 200 with `queued` status, null conclusion, and zero
jobs/check runs. Normal cancellation returned HTTP 409 with the exact message
`Cannot cancel a workflow run that has not been queued yet.` Force-cancellation
returned HTTP 409 with the same message. Final reads still showed queued/null/
zero state, with no timestamp change.

| run | suite | created | initial HTTP 200 | normal cancel | force-cancel | final state |
|---:|---:|---|---|---|---|---|
| `32985134450` | `89353010038` | `2026-08-26T15:26:20Z` | queued / null / zero | HTTP 409, exact message above | HTTP 409, same message | queued / null / zero; timestamp unchanged |
| `32984965998` | `89352428140` | `2026-08-26T15:23:45Z` | queued / null / zero | HTTP 409, exact message above | HTTP 409, same message | queued / null / zero; timestamp unchanged |
| `32984867843` | `89352110318` | `2026-08-26T15:22:25Z` | queued / null / zero | HTTP 409, exact message above | HTTP 409, same message | queued / null / zero; timestamp unchanged |

This attempt establishes neither terminality nor GitHub Support confirmation.
No other runs, suites, runners, files, workflows, or policies changed. The
cleanup gate remains open.

## Run state

| repository | run | status | conclusion | updated | jobs |
|---|---:|---|---|---|---:|
| `tailrocks/velnor` | `33010337075` | completed | cancelled | `20:28:41` | 0 |
| `tailrocks/velnor` | `33009178017` | completed | failure | `20:32:35` | 13 |
| `tailrocks/velnor` | `33009178083` | completed | failure | `20:21:18` | 16 |
| `tailrocks/velnor` | `32988760007` | completed | failure | `16:32:56` | 16 |
| `ChainArgos/java-monorepo` | `33010149626` | completed | cancelled | `20:33:59` | 3 |
| `jackin-project/jackin` | `32965105931` | completed | failure | `13:07:44` | 19 |
| `jackin-project/jackin` | `32962624253` | completed | failure | `15:01:50` | 17 |
| `jackin-project/jackin` | `32958445805` | completed | failure | `10:34:25` | 5 |
| `Velnor` | `33011217619` | completed | cancelled | `20:36:41` | 0 |
| `Velnor` | `32989529327` | completed | cancelled | `20:37:17` | 15 |
| `Velnor` | `32987778460` | completed | cancelled | `20:37:18` | 15 |
| `tailrocks/velnor` | `33012336003` | 404 / unknown (prior/indexed: queued) | unknown | not recorded | not recorded; check suite `89435047597` |
| `tailrocks/velnor` | `33012336003` | 404 / unknown (prior/indexed: queued) | unknown | not recorded | not recorded; check suite `89435047597` |

## Unresolved queue objects

The following IDs remain unresolved and remain hard gates:

`33012336003`, `89435047597`, `32985134450`, `89353010038`,
`32984965998`, `89352428140`, `32984867843`, `89352110318`.

The current Velnor run and suite returned HTTP 404, with current status
unknown; the prior/indexed state recorded the run as queued. That is not
treated as resolution without GitHub Support confirmation. The three ChainArgos runs and
suites retain the queued/null/zero-check-run state above. The exact-target
remediation attempt changed no other runs, suites, runners, files, workflows,
or policies. Prior cleanup-attempt evidence records earlier cancellation
requests and the removal of eight stale validation-owned registrations; the
unresolved objects and all related hard gates remain unchanged.

## Plan disposition

The production-readiness plan remains incomplete. The reconciliation does not
clear the terminal-run, runner-registration, admission/readiness, dispatch, or
verification gates. No further mutation occurred after the exact-target
normal/force cancellation attempts, and no dispatch/rerun/rerequest/deletion/
runner/workflow/policy mutation occurred.

## Current read-only refresh — 2026-08-26T23:30:38Z

Successful read-only facts captured in this refresh:

- ChainArgos runs `32985134450`, `32984965998`, and `32984867843`, with check
  suites `89353010038`, `89352428140`, and `89352110318` respectively, remain
  queued with null conclusions and zero checks; their timestamps remain
  unchanged.
- Velnor run `33012336003` and check suite `89435047597` return HTTP 404.
- Runner group `4` (`velnor-trusted`) policy is unchanged, and its
  `/runners` endpoint reports `total_count=0`.

A repo-wide runner/noncompleted-run listing was not captured because local
`rtk` proxy/shell argument parsing failed. Prior runner-registration evidence
therefore remains historical; no current-state inference is made from it.

No mutation occurred in this refresh. The production-readiness gates remain
blocked.

## Ownership/state audit — 2026-08-26T23:38:06Z

Supplied investigator facts, recorded without inference or mutation:

- Velnor run `33023493172` completed with conclusion `cancelled`.
- Velnor runs `33023384527` and `33023384501` are active release-adjacent
  push runs on `v0.1.228`. They are not campaign-owned and are unsafe to
  mutate.
- Velnor run `33023383914` is terminal success.
- ChainArgos `java-monorepo` run `33019314096` is a queued automatic PR run.
  It is campaign-related but not provably campaign-owned and is unsafe to
  mutate.
- Stale candidate `32985134450` (check suite `89353010038`), stale candidate
  `32984965998` (check suite `89352428140`), and stale candidate `32984867843`
  (check suite `89352110318`) are explicitly enumerated. The evidence proves
  only that these are old zero-job runs on obsolete SHA
  `48f687259bed568409ac4a6308a2fc5f2d970b82` and that prior exact-target
  normal/force cancellation attempts returned HTTP 409. It does not prove
  current campaign ownership. Classification: `unowned/ownership unresolved`,
  not campaign-owned. Do not recommend cancelling them without explicit
  ownership.
- Protect ChainArgos run `33019314096` and active Velnor runs
  `33023384527` and `33023384501`; they are unsafe to mutate.
- Current Velnor registrations total five: three online/idle microVM IDs
  `15342`, `15339`, and `15341`, plus two offline IDs `15327` and `15330`.
  No validation label or prefix was captured.
- ChainArgos organization runners total zero.
- The safe set for runner deletion is empty; no runner deletion is recommended.

Jackin and the current repo-wide list were not captured in this partial audit;
prior evidence is historical. No ownership, current state, or safety is
inferred beyond the supplied facts. No mutation occurred. Cleanup remains
open because active/queued runs remain and there is no current Sentry proof.

## Current partial recheck — 2026-08-26T23:49:51Z

At capture, noncompleted runs were observed as follows:

| repository | noncompleted runs at capture |
|---|---|
| `tailrocks/velnor` | `33024640131` in progress; `33024640078` queued; `33024639794` in progress |
| `ChainArgos/java-monorepo` | `33019314096`, `32985134450`, `32984965998`, and `32984867843` queued |
| `jackin-project/jackin` | none |

Historical Velnor runs `33023384527` and `33023384501` are now completed with
failure conclusions. The newly observed runs are not claimed to be
campaign-owned.

Runner registrations, validation labels, and runner-group `4` membership were
not captured in this partial recheck. No inference is made from their absence.
No cleanup action is safe on this evidence. No mutation or dispatch occurred;
the no-dispatch gate and production-readiness gate remain open.

## Root-cause proof — 2026-08-27T00:16:00Z

Durable evidence for `tailrocks/velnor` PR run `33024674982`, SHA
`e79ed1969e9b05081c4af4e5f0f7c2d295590883`: the GitHub lane failed test
`controller_observes_live_session_and_executor_before_ready_proof` with
`session_live=false` and `executor_proven=true`. The contract and `ci-required`
checks were downstream of that failure; the Velnor lane was skipped. This is a
test/readiness failure record, not live production proof.

Root cause: direct test slot launches omitted `--generation`, while strict
procfs identity validation ignored the heartbeat. The architecture therefore
allowed a slot to appear executor-proven without proving a live session for the
same generation and heartbeat identity.

Structural fix: make `SlotArgs` generation required and use explicit test and
systemd invocations, so every slot launch carries generation identity and the
readiness proof observes the strict procfs/heartbeat contract.

Local regression proof: focused `cargo nextest` passed 3/3, and formatting
passed. No GitHub Actions rerun was performed because cleanup and external
access blockers remain. Residual gate: the Actions result and live
admission/readiness proof remain unresolved; do not claim live production
proof or production readiness from this evidence.
