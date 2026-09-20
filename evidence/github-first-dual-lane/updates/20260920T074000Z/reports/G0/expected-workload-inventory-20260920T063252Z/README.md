# G0 expected workload inventory

This external, source-only inventory covers exactly six repositories and is intended for the G0 checker join. It records expected logical workloads, jobs, platform/architecture and provider eligibility, dependency/reusable-action/child/release/package edges, explicit missing/unknown/excluded obligations, and the five PR identity/churn records captured by the raw workflow/ruleset audit.

## Artifacts

- Machine inventory: `inventory.json`
- Raw workflow/ruleset capture: `../fleet/g0-current-workflow-ruleset-raw-capture-20260920T052347Z/`
- Accepted goal: `../../../velnor3/velnor-github-first-dual-lane-goal.md`
- Velnor base: `abe9ad82a2d4d01b706bbc6122ab6ccb150faad9`

The six source snapshots are detached, clean trees pinned in `sources/`. The inventory is derived from those source trees plus the raw capture; no successful run is treated as workload evidence. All workflow records remain `source_only_no_execution`; no dispatch, gate, migration, or source/workflow edit was performed.

## Validation

From the workspace:

```sh
jq empty dual-lane-evidence/G0/expected-workload-inventory-20260920T063252Z/inventory.json
```

The audit also checked every recorded source-path blob SHA against its pinned detached tree, every tree count/listing digest, and normalized raw-capture churn against `inventory.json.churn.records`.
