# Fleet operations plans

Current plans for organization-level runner admission, routing, and fleet
policy. These plans preserve the unified-CI contract and do not add commands to
the retiring `velnor-runner` surface.

## Plans

Execute each item through the durable controller in
[`../goal-execution/README.md`](../goal-execution/README.md). Plan 039 remains a
separate P0 security goal; live apply still requires its exact reviewed digest
and operator authorization.

| Plan | Title | Priority | Depends on | Status |
|---|---|---:|---|---|
| [039](039-org-jit-multi-repo-fleet.md) | Reconcile restricted organization JIT fleets | P0 | Unified-CI contract | IN PROGRESS |

## Boundary

- Unified-CI generation owns repository classes, workflow paths, lane defaults,
  and trusted refs.
- This category owns exact GitHub runner-group admission and drift proof.
- [`velnorctl-migration`](../velnorctl-migration/README.md) owns product CLI,
  service, API, and later fleet-view work.
- Public unmerged contributor code remains GitHub-hosted until lower-trust
  Velnor isolation is live-proven.
