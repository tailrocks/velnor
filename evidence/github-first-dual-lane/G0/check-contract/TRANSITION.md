# G0 check contract — tailrocks/velnor

Observed `2026-09-19T16:37:34Z`, source/main tip `abe9ad82a2d4d01b706bbc6122ab6ccb150faad9`.

## Active rules

Repository ruleset `19573071` (`protect-main`) is active for `~DEFAULT_BRANCH`.
Required status contexts are exactly:

- `DCO`
- `ci-required`
- `Policy`

`strict_required_status_checks_policy` is `false`. The ruleset stores no
integration IDs, so context names alone do not prove producer identity.
Deletion and non-fast-forward protection are active. The pull-request rule
requires zero approvals, dismisses stale reviews on push, and enables the
extra approval rule for unattributed changes. Repository-role actor `5` has
an `always` bypass.

Classic branch protection is absent (`404 Branch not protected`); the ruleset
is the effective main contract.

## Producer identity and observed IDs

`DCO` is produced by DCO-2 app `974774` (`dco-2`). `ci-required` and `Policy`
are produced by GitHub Actions app `15368` (`github-actions`). Current main
check runs for those contexts are all GitHub Actions; no Velnor runner job
should be relabeled as a successful required context.

Main tip `abe9ad82` has no PR association. The latest inspected manual run is
`35430875046` (workflow_dispatch, failed). Its required check runs are:

| Context | Check run | Suite | App | Result |
| --- | ---: | ---: | ---: | --- |
| `ci-required` | 105867022907 | 95952458471 | 15368 | failure |
| `Policy` | 105865096888 | 95952458471 | 15368 | failure |
| `Control / Required` (producer detail, not a required ruleset context) | 105867032979 | 95952458471 | 15368 | failure |

The main DCO suite `95704614706` is still queued with zero check runs. A
direct main run does not carry a PR association.

## Current PR candidate semantics

- PR953: declared base `main@abe9ad82`, head `af31b644`, stored merge SHA
  `f154d3b4`. The workflow checked out synthetic integration SHA `f154d3b4`
  with parents `abe9ad82` and `af31b644`; run `35453601367` remains queued.
  DCO `105924879553` succeeded; Policy `105924883126` failed; `ci-required`
  has not appeared in the current queued suite `96009929573`.
- PR954: API declares base ref `codex/s2-desktop-task-release` at `8964876f`,
  head `f16592ea`, merge SHA `2860ef3d`. The current base ref tip is
  `af31b644`; the actual synthetic merge `2860ef3d` has parents `af31b644`
  and `f16592ea`. The workflow selection step reported base `8964876f`, so
  the declared base, selected base, current ref tip, and actual merge parent
  must remain separate evidence fields. Run `35454970877` remains queued.
  DCO `105928522218` succeeded; Policy `105928525183` is in progress;
  `ci-required` has not appeared in queued suite `96013421316`.
- PR952: head `a5c1c0bd`, stored merge SHA `b5b2a124`; DCO succeeded,
  Policy succeeded, and `ci-required` failed in its last observed checks.
- PR948 (draft): head `ab82b087`, no merge SHA; DCO and Policy succeeded,
  `ci-required` failed in its last observed checks.

Check-suite `pull_requests` records the PR base/head association. A required
check claim must also carry the workflow event and actual checked-out SHA;
the PR head SHA alone is insufficient for a pull-request run.

## Hosted-only transition sequence

1. Preserve the neutral required names `DCO`, `ci-required`, and `Policy`.
2. For each recovery candidate, snapshot PR API base/head, merge SHA, suite,
   check-run IDs, app IDs, workflow run, and actual checkout merge SHA.
3. Repair the candidate until hosted GitHub Actions emits successful
   `ci-required` and `Policy` for the exact integration SHA, and DCO-2 emits
   successful `DCO` for the same candidate. A wrapper or scheduler green
   result is not sufficient.
4. Merge only after those checks are complete; then require fresh checks on
   the resulting main SHA. Keep the post-merge evidence separate from PR
   merge-ref evidence.
5. Preserve unrelated required checks and ruleset protections. Add a second
   provider context only after it has a reliable producer, then update the
   ruleset and workflow together. Never claim a queued or skipped Velnor job
   as a hosted success.

## Decisive current blocker

PR953 job `105928872280` failed at
`2026-09-19T16:30:26Z`:

```text
parameterized_callees_resolve_to_the_pre_parameterization_cache_keys
assertion `left == right` failed:
github-hosted-rust-policy -> ci-unit-rust.yml#verify-github-hosted [mbx]: primary key changed
left:  velnor-mbx-v3-fe8982552e68-...
right: velnor-mbx-v3-ec62b8b58bab-...
```

Raw job evidence:
<https://github.com/tailrocks/velnor/actions/runs/35453601367/job/105928872280>

This is forwarded to the cache-semantics owner. No rules, workflows, PRs, or
runs were modified by this investigation.
