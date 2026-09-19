# G0 records completeness review

Verdict: **INCOMPLETE / BLOCKED**. No G0 pass.

Audit time: `2026-09-19T19:17:40Z`. Effective reviewer settings: `gpt-5.6-luna`, reasoning `max` (verified).

Subject: committed records branch `codex/github-first-records` at `92f6ff62d80bcd8e1edb51135277d6d434982cd5`, clean worktree. Machine-readable result: [records-completeness-review.json](./records-completeness-review.json).

## Scope and revision facts

The committed `fleet.json` contains exactly 32 unique rows. All 32 declare `main`, a non-null lowercase 40-hex SHA, and a syntactically valid UTC observation timestamp. This proves static scope shape only.

Independent external `main-revisions.tsv` contains 32 unique rows at `2026-09-19T17:05:57Z`; all branch names, SHAs, and timestamps are valid. Thirty-one committed row SHAs match it. One is stale:

| repository | committed SHA | independent current SHA |
| --- | --- | --- |
| `jackin-project/homebrew-tap` | `f1669391582f92c95da1aa5958de4397dfedca09` | `2281ae9c95dfcb5bf82dfd025fdf58a50658b1d1` |

“Source-bound” here means repository identity exists in the independent snapshot and branch/SHA match; result: **31/32**, not 32/32.

Latest external snapshot is `17:05:57Z`; audit time is `19:17:40Z`. No current-now GitHub refresh was run. Treat external “current” claims as stale unless refreshed.

## Committed fleet row completeness

The static rows have control metadata (`owner`, `reviewer`, `gate_status`, `blocker`, `next_action`, `evidence_status`) in all 32 rows. Acceptance data is absent:

| group | required fields | nonempty rows | complete rows |
| --- | --- | ---: | ---: |
| PR | number/head/base/tested-merge/merge-group | 0 for every field | 0/32 |
| checks | required contexts/expected jobs/actual IDs/conclusions/logs/child links | 0 for every field | 0/32 |
| workflow/run | path/revision/event/run/attempt/url/trigger/checkout/provider/runner/host | 0 for every field | 0/32 |
| workload | expected IDs/platform+arch/provider/exclusions | 0 for every field | 0/32 |
| release/install | release, assets, APT, Homebrew, install identity/result | 0 for every field | 0/32 |
| access | structured row field | field absent from row schema | 0/32 |
| dependency | typed graph fields/edges | fields absent from row schema | 0/32 |

External `inventory.md` says all 32 repositories were readable at `16:34:19Z`; that is historical access evidence, not a current per-row access field.

## PR/check snapshots

The historical inventory claims 78 open PR rows (75 ready, 3 drafts) at `16:34:19Z`. Later `pr-checks.tsv` has 77 unique PR rows across 12 repositories at `16:52:43Z` (74 ready, 3 drafts). This is the expected 78→77 reconciliation, not proof that all current PRs are covered now.

The 77 PR rows have valid timestamps, head/base/default/rollup SHAs, numeric check counts, required-like fields, and app IDs. Twenty-four have failed checks, two pending checks, 64 skipped checks, and 65 incomplete flags. Two rows report paginated contexts. The file is a PR/check snapshot, not a complete 32-row workflow/run/workload ledger; workflow revision, runner identity, child-run links, provider proof, and exact required-check authority remain absent.

External file SHA-256:

- `inventory.md`: `42fac3779bc41b48b6850cdab5cc6deccd5d8ba693a68f16f91791ad4623b16b`
- `main-revisions.tsv`: `2e79bd50201c488376c6d25fe5c781927a3752d5801d9fc3c9351c9da981d915`
- `pr-checks.tsv`: `54729bba3e6b6b1c33c03251e50e7026226c0eb26375f60c3f1780b4e9bec370`

## Workload enrichment

[workloads-full.json](./workloads-full.json) is an external, read-only projection derived from `G0/workload-matrix.json` (SHA-256 `f6d91897469474a28dc754d6f50521af2667ad4ef3c2694553e2afb01cd98820`) and bound to records commit `92f6ff62d80bcd8e1edb51135277d6d434982cd5`. It covers all 32 rows and records:

- 115 source-observed expected workload IDs (86 unique), with status/notes preserved;
- all 43 observed scanner IDs (41 unique), without fabricating IDs for rows with none;
- 50 platform/architecture/provider rows and 17 native obligations;
- provider trust eligibility, explicit native exclusions, misroutes, unsupported blocks, and category evidence;
- revision binding to the independent 17:05 main snapshot;
- historical-only access status, unknown required-check authority, and explicit empty typed dependency graphs (`workload→child`, `workload→release`, `workload→package`, `workload→required_check`).

All 32 workload rows have expected workloads, platform data, provider eligibility, and unsupported-block records in this projection. These are source-observed/configured obligations, not generated coverage or execution success. Gate statuses remain blocked or partial.

## Required next action

Refresh all 32 repositories and 77/78 PRs against live GitHub; repair the one stale default SHA; populate per-row workload/check/workflow/provider/host/run/child evidence; add typed dependency and access records; complete paginated contexts; then run the fail-closed checker and independent review. Do not mark G0 passed from the static 32-row shape or the external inventory existence alone.

