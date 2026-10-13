# Coordination resumption handoff

## Deliverable and location

The canonical continuation plan is [CAMPAIGN_PLAN.md](CAMPAIGN_PLAN.md).
This handoff records the implementation boundary and restart position; it is
not a second plan or an operational completion claim.

- Source checkout: `/Users/donbeave/Projects/velnor-bastion`.
- Isolated local clone: `/Users/donbeave/Projects/velnor-bastion-coordination.dL1Dm3`.
- Branch: `codex/campaign-coordination`.
- Base commit: `054d13710e49e12c0df196fb663aef6a188e388b`.
- Changes are uncommitted and unpushed. The branch still points at the base.
  Preserve this working directory; cloning the branch will not recover these edits.
- The clone has its own Git metadata and object copies. Its origin is the local
  source checkout; no remote GitHub fork or publication was created.

The workstream is coordination-only. CURRENT_STATE.md, CAMPAIGN_LEDGER.md,
existing deployment/monitoring scripts, playbooks and hosts.ini are preserved.
No host, Docker provider, product repository or consumer repository was modified.
No new live operational evidence was collected by this workstream.

The final source-checkout status showed concurrent edits to `CAMPAIGN_LEDGER.md`,
`docs/CURRENT_STATE.md`, `scripts/monitor-permits.sh` and
`scripts/monitor_permits.py`. This workstream did not make or overwrite them.
They are not silently imported into this isolated base snapshot. Reconcile them
explicitly when integrating; the new user-supplied refresh is already recorded.

## Resume position

Current campaign status: `blocked`. Earliest unverified gate: G0.
Preview selection: `blocked`, with no lock ID or selected component pins.
All consumer lifecycle evidence remains pending; both-host qualification is blocked.
The coordination artifacts can validate successfully in this state.

The latest user-supplied evidence is preserved in
[integration-context-20260923.md](evidence/integration-context-20260923.md).
It records current product main `6391a0ce53b7e666d1bfb391083011622158f4f3`,
preview run `35797775098` in progress, and stale rolling target `f7bc191`.
Never select that stale prefix. Do not infer the running preview's source SHA,
attempt, success or published artifacts from current main.

Bastion's 2026-09-22T23:44Z refresh confirms Docker 29.8.1, stable 0.1.274,
16 slots but zero ready, GitHub unreachable, quota settings present and daemon
inactive. Mac's 2026-09-23 MYT report has M5 Max arm64 / OrbStack arm64,
no supervisor, state N=1 versus monitor N=4. Both values are observations,
not a chosen capacity. Exact Mac observation time was not supplied.

Earlier detailed facts retain their original timestamp and source. Archived
completion claims, PRs, abbreviated SHAs and run IDs are explicitly historical.
G2 means the first Scale Set pilot in the recovered objective; the ledger's
different G2 label must not replace it.

## Bounded continuation

1. Read the canonical plan and all five state files. Run the local validator
   and focused tests using the isolated-environment commands in the plan.
   Check the uncommitted diff and preserve the source hashes in the evidence index.
2. Within coordination scope, reconcile newly supplied observations and pending
   reviews. Add a secret-free source record with honest capture precision;
   update only affected facts, blockers and evidence references. Keep the source
   record hash current after an actual reviewed change, never to conceal drift.
3. Keep G0 open until authorized current repository/access/workload and full
   installed-host inventories exist. Route the Mac supervisor/N mismatch and
   bastion URL/credential/routing/control/persistence/quota defects to their
   responsible product/host workstreams. The plan records future work, not
   authorization to execute it here.
4. After separately authorized repairs and verified preview publication, follow
   the plan's Mac-first consumer sequence. A completed workflow or coherent
   package alone does not release the qualification gates.

Do not execute commands copied from the historical handoff or deployment
runbooks under this workstream. Do not mark historical green runs current.
Do not commit, push, publish or mutate hosts while this scope remains in effect.

## Validation contract

Local verification completed at 2026-09-23T00:06:09Z with Python 3.14.7 and
jsonschema 4.26.0: all five state files and six schemas passed; 35 focused tests
passed. The validator also passed when invoked from outside the checkout.
Protected source/deployment files in this clone match the captured base commit.

The validator reads only local state, schemas and cited source records. It checks
JSON syntax (including duplicate keys and non-finite numbers), Draft 2020-12
schema validity, exact references/order, hashes, historical-proof boundaries and
preview rejection rules. Focused tests exercise invalid evidence, stale preview
selection, premature promotion, ordering, credentials and malformed input.
Checks do not reproduce live GitHub, Docker, package, host or test execution.

State files:

- `state/campaign-state.json`: canonical pointers, G0-G8 task graph and blockers.
- `state/preview-lock.json`: unresolved selection, observed inputs, stale exclusions.
- `state/hosts.json`: dated observed/historical facts and current blockers.
- `state/repository-rollout.json`: exact consumer order, lifecycle and qualification.
- `state/evidence-index.json`: source methods, hashes, capture precision and citations.

Schemas are `schemas/common.schema.json` plus one schema matching each state
filename. The command is `python scripts/validate-campaign-state.py`; an optional
`--root /absolute/checkout` validates another isolated copy. Python 3.10+ and
`jsonschema>=4.22,<5` are required. A successful local validation never means
the operational campaign is complete.

## Runtime and review limits

Requested model `gpt-5.6-luna` is unavailable, as reported by the user.
No model-selection or reasoning-level control is exposed to this workstream;
strongest-available/max-reasoning configuration cannot be independently verified.
The work proceeds in the supplied runtime without claiming a model switch.

No subagent/delegation tool is exposed. No agent IDs or independent review are
fabricated. Operational gate author/verifier identities remain unassigned.
Author-run validation and adversarial fixtures are local implementation checks;
independent operational acceptance remains outstanding.
