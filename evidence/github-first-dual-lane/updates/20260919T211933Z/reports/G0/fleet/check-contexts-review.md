# G0 check-contexts-full review

Review time: 2026-09-19T19:46:29Z. Read-only review; no source or GitHub mutations.

## Artifact

`G0/fleet/check-contexts-full.json`

- SHA-256: `1a51f8a276c912c6f03f3bcb749d1f5845b52c0551cdff6254c8e90c0c78901e`
- 5,778,314 bytes; 140,185 lines; JSON valid.
- Schema: `g0-fleet-check-contexts-full/v1`.
- GitHub REST API `2022-11-28`; 788 recorded calls; declared read-only.
- Source capture: `2026-09-19T19:15:10.439Z` through reconciliation `2026-09-19T19:28:02.940Z`.

## Passed structural checks

- Goal manifest comparison is exact, including order: 32/32 unique repositories.
- All 32 default branches are `main`; internal ref/source identity checks have zero mismatches.
- 76 open PR records across 12 repositories; 20 repositories have zero PRs but still have main-branch observations.
- Initial/final PR counts are both 76; all per-repository churn counters are zero.
- PR numbers are unique, all records are open, head/base identity fields exist, and observed source SHA equals PR head SHA for all 76.
- Pagination metadata is present and count-consistent. Multi-page observations are retained (two main and two PR check-run endpoints); all observed endpoint results are HTTP 200/access `ok` except explicitly classified branch-protection 404s.
- 3,010 check rows and 372 workflow rows have source SHA consistency. All workflow rows have event and source identity. All 2,879 GitHub Actions check rows have workflow-run IDs that resolve to local workflow rows.
- Required policy checks (54) carry ruleset provenance.

## Protection semantics

31 branch-protection endpoints are represented as `state=not_protected`, `api.access=not_found`, HTTP 404, error `Branch not protected`; one endpoint is protected/HTTP 200. No 403/unknown/unavailable result is silently turned into “not protected.” Ruleset calls are separately retained. Consumers must preserve this distinction and must not erase ruleset requirements because branch protection is absent.

## Blocking limitations for complete gate evidence

1. Every main and PR check-run query declares `filter=latest`. This is not an all-run/run-attempt inventory and cannot prove no prior failed/cancelled run was omitted before a newer result. This fails the no-newest-green requirement.
2. 131 external-provider checks have no workflow/event binding: DCO-2 (76) and SonarQubeCloud (55). They retain app identity and source SHA, but are not hosted workflow bindings.
3. Final open-PR responses are represented by endpoint metadata/counts and churn counters only; final PR identity rows are not retained, so the no-churn final set is not independently replayable from this artifact alone.
4. Eleven PRs have no tested merge SHA. Keep those merge results unknown, not successful.

## Read-only live samples

At `2026-09-19T19:46:08Z`–`19:46:16Z`:

- `tailrocks/velnor` default `main` matched artifact SHA `e713841bdb9c33d853b7a9af88ceac924af1b3b6`.
- `jackin-project/homebrew-tap` default `main` matched artifact SHA `34ae3239f3f6760a11d02bec85b2c07cd454eb2a`.
- Tap PR 492 head matched artifact SHA `9255223b26d4a7340d92ec20fef7f95474aa4a58`.
- Velnor PR 954 had advanced from captured head `b93157f7f0971c73d1964447b5570c39aca6bf5e` to live head `7609366f5ad530c12a87addf98d0c49448009362`; live checks were 70 versus captured 2. This confirms the artifact is historical, not current-state proof.
- Velnor branch-protection API returned HTTP 404 with `Branch not protected`, matching the artifact's explicit semantics.

## Ingest verdict

Approve ingest only as a historical, immutable external evidence snapshot with the limitations above attached. Do not use it to pass G0/G7, to assert current PR identities after the capture window, or to claim complete all-run/no-newest-green evidence. `summary.no_gate_claim=true`; G0 remains pending.
