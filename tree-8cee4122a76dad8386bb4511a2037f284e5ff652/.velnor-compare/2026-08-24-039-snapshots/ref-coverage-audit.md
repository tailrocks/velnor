# Runtime-ref coverage audit — Plan 039 prerequisites-first (2026-08-24)

Question: before flipping `restricted_to_workflows=true` on the Velnor runner
groups, does observed runtime traffic fit the ledger's admission shape
(`fleet/release-refs.toml`, currently `refs/heads/main` only)?

## Method (read-only)

- Source: `gh api /repos/{owner}/{repo}/actions/runs?per_page=100` per estate
  repository (org-level `/orgs/{org}/actions/runs` returns HTTP 404 for this
  token; repo-level endpoint used instead), runs filtered client-side to the
  exact `workflow_path` values admitted by `fleet/release-refs.toml`.
- For each sampled run: `/actions/runs/{id}/jobs` inspected for a job whose
  `runner_group_name` or job labels contain `velnor` → run counted as
  executed on a self-hosted Velnor runner.
- Window: most recent ~100 runs per repository (≈60 days of activity).
- Ref-shape classification from run metadata (`event`, `head_branch`,
  `head_sha`): PR events → `refs/pull/N/merge`; tag-looking branches or
  release events → `refs/tags/*`; `main` → `refs/heads/main`; else
  `other-branch`.
- Raw evidence: `runtime-ref-hits.tsv` in this directory (484 rows:
  repo, workflow, sha, event, head_branch, ref class, group|labels).

## Coverage table

| Org | Sampled | refs/heads/main | refs/pull/N/merge | other-branch | refs/tags/* | Denied by current ledger |
|---|---|---|---|---|---|---|
| ChainArgos | 74 | 59 | 11 | 4 | 0 | 15 |
| jackin-project | 223 | 135 | 70 | 18 | 0 | 88 |
| tailrocks | 187 | 168 | 8 | 10 | 1 | 19 |
| **Total** | **484** | **362 (74.8%)** | **89 (18.4%)** | **32 (6.6%)** | **1 (0.2%)** | **122 (25.2%)** |

Repositories covered: 31 distinct `owner/repo` pairs across all three orgs.
Events seen on Velnor runners: workflow_dispatch 199+, push, schedule,
pull_request.

Denied-shape branch names observed on Velnor runners (top): `ci/push-github-lane`
(8), `velnor-estate-standard` (4), `ci/velnor-lane-routing` (4),
`ci/velnor-fleet-conversion` (3), `chore/publish-reusable-artifact-scope-pin`
(3), plus assorted `fix/*`, `ci/*`, `chore/*` working branches.

## Verdict

**GAPS.**

- `refs/pull/N/merge` — 89 runs would be DENIED (largest gap; every
  pull_request-triggered Velnor-lane job).
- `other-branch` — 32 runs would be DENIED (feature/integration branches,
  including this repository's own program branch `velnor-estate-standard`
  and CI-routing integration branches that are part of the estate migration
  itself).
- `refs/tags/*` — 1 run would be DENIED (tailrocks).

Flipping `restricted_to_workflows=true` with the ledger as-is would have
denied ~1 in 4 of the last ~60 days of real Velnor executions, and non-matching
jobs queue forever silently ("Waiting for a runner", control-plane enforced,
no documented drain grace). Prerequisite confirmed: regenerate the allowlist to
cover the denied shapes (PR merge refs at minimum) BEFORE enabling restriction.
