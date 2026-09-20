# Corrected canonical source workload index — 2026-09-20T00:29:55Z

Status: source-derived corrected index only. `gate_status=not_evaluated`; attestation none; no G0 claim.

## Revision

- Supersedes historical index `G0/fleet/workload-contract-index-20260920T000017Z.json` (SHA-256 `75000aca431f4e195a39f605eea846bc0ba6aed58e3bbc38e94e42009b12b269`). Historical index preserved.
- Corrected supplements: `fleet-10`, `distribution-8`; unchanged: `skills-8`, `remaining-6`.
- Source execution, dispatch, install, release, publication, and Velnor launch: none.

## Exact set

- union: 32; expected: 32; exact disjoint: `true`
- duplicates within supplements: none; cross-supplement overlap: none.
- `distribution-8`: 8 rows
- `fleet-10`: 10 rows
- `remaining-6`: 6 rows
- `skills-8`: 8 rows

## Corrected immutable inputs

- `distribution-8`: `G0/distribution-workload-contract/workload-contract-20260920T001239Z-corrected.json`; SHA-256 `1132d45af4a583c88925302d06d2c0d330b5eee0e01131a4cc41df94840359d4`; captured `2026-09-19T23:33:08Z`
  - correction metadata retained: `correction`
- `fleet-10`: `G0/fleet/workload-contract-10-20260920T001932Z-corrected.json`; SHA-256 `eb1ae8fffaa35e7a91124435c7d2bc43aa23eb97e68793635f52bfd814acbf71`; captured `2026-09-19T23:33:41Z`
  - correction metadata retained: `artifact_revision`
- `remaining-6`: `G0/skills-adapter/workload-contract-remaining-six-20260919T233818Z.json`; SHA-256 `bdac4fc99a99d07e4d862d896b1c6378709e14788fa8c7d4d03b12c173f4d1dc`; captured `2026-09-19T23:41:44Z`
- `skills-8`: `G0/skills-adapter/workload-contract-20260919T225313Z.json`; SHA-256 `745921e2df5ffc286e63bbccb2ff8e6f328639a1b27c5198c784fbd896b872f8`; captured `2026-09-19T22:53:13Z`

Input pre/post SHA-256 matched. Full source-tree path/blob inventories and source pins remain in JSON; source facts were not rewritten.

## Corrections retained

- Fleet correction: typed external Git dependency fields (`external_dependencies`, `external_nodes`, `external_edges`), source-pinned platform rows, and corrected per-path blob pins.
- Distribution correction: Velnor Homebrew architecture remains source-unknown; holla APT child workflow/plan corrected; reuse note remains source provenance.

## Capture churn

- Historical Velnor source churn retained: `1048337062ea625fada1b4f7c07f2feed75f60c7` → `d20d4d1d17590cca85b501d982cbaad70d42c641`, `stable_across_window=false`; source facts bind to closing `d20d4d1d17590cca85b501d982cbaad70d42c641`. No Velnor execution implied.
- Explicit stable: 15; changed: 1; churn not recorded: 16.

## Schema/unknown limits

- Dependency/workload schemas remain source-specific and unnormalized; corrected fleet external dependency fields are not projected into other supplements.
- No source supplement supplies execution, job/result, artifact, provider transcript, or Velnor output.
- Missing workflows, API/tree mismatches, required-check App IDs, branch-protection limits, and source-only dependency graphs remain unknown or explicit missing; none become success.

Handoff: `g0_records`, `g0_checker`, `g3_distribution_consumers`. Publication deferred to stable batch.

JSON artifact: `G0/fleet/workload-contract-index-20260920T002955Z-corrected.json`; SHA-256 `2b3bf88b42f291a40dcc2d6eb65a489d72d2a5bdcf9ab094a69a43e01f750c9c`.
