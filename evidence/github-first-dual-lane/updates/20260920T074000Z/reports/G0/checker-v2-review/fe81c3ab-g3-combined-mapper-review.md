# G3 combined mapper review: `fe81c3ab66966e7fc19836ae28d5203f5ba6554f`

Date: 2026-09-20.  Scope is the combined offline mapper only.  This is not
approval of G0, live authority, release, install, or publication.

## Verdict

**Reject as acceptance-ready.**  The exact tree is clean and remote-equal, and
the bounded unit/tool checks pass.  The mapper has useful fail-closed checks,
but an authentic non-empty collector result cannot currently map successfully,
and several provenance obligations are enforced only by later checker code (or
not at all at this boundary).

## Exact source and commands

- Isolated detached worktree: `/private/tmp/g3-combined2-review-fe81`.
- `HEAD` and `origin/tmp/g3-combined2`: `fe81c3ab66966e7fc19836ae28d5203f5ba6554f`.
- Parent compared for this review: `63e8d61fc199acecbfb25819127ff8786bb11347`.
- Worktree status: clean; no source or owner fixture changes made.
- `rtk cargo test --locked --all-features --package velnor-tools`: **354 passed** (2 suites).
- `rtk cargo test --locked --all-features --package velnor-tools g0_mapping -- --nocapture`: **8 passed**, 346 filtered.
- `rtk cargo test --locked --all-features --package velnor-tools artifact -- --nocapture`: **5 passed**, 349 filtered.
- `rtk cargo clippy --locked --all-features --package velnor-tools -- -D warnings`: **pass**.
- `rtk cargo fmt --all -- --check`: **pass**.

No `live-collection.json`, `live-sample.json`, or `binding-capture.json` was
present under `dual-lane-evidence` or the project tree.  The available G0 raw
capture is a shell/API ledger, not a deserializable `LiveCollection`; therefore
no claimed real-input mapper regression was possible.  This review does not
turn that absence into evidence of successful live collection.

## Bounded contract results

| Obligation | Result | Exact evidence |
|---|---|---|
| Canonical output schema and read-only mode | Pass, shape only | `g0_live_mapping.rs:440-458` emits schema `2`, mode `read_only`. |
| Fixed repository census | Pass, shape only | `g0_live_mapping.rs:253-300` requires exactly 32 manifest/live rows and exact canonical names, rejecting duplicates/substitution. |
| Effective model and Luna/max agents | Pass, typed raw boundary | `g0_live_mapping.rs:50-121,200-237`; model/workload wrappers parse raw bytes, require own raw IDs, and enforce Astra/low + Luna/max. |
| Local/provider raw merge | Pass, measured shape only | `g0_live_mapping.rs:302-368,623-659` rejects duplicate/conflicting IDs and verifies safe byte digest/length plus canonical safe/original storage-ref shape. External CAS reopening remains producer-owned. |
| Ruleset/PR/check nested endpoint and kind | Pass, bounded | `g0_live_mapping.rs:970-996,1534-1549,1699-1717` binds exact REST paths/kinds and rejects repeated/missing nested raw IDs. |
| Check attempt query | Pass | `github_acquisition.rs:672-687,787-800` emits `filter=all`; `g0_live_mapping.rs:1466-1512,1718-1723` rejects absent/latest/non-all queries; focused test covers latest. |
| API head versus actual checkout | Pass, fail-closed shape | `map_pull_request`/`map_check` require `actual_checkout_sha`; `LiveCheckoutObservation::api_head_only` cannot satisfy it. Live authority is still unavailable (`live_authority.rs:123-161`). |

## Blocking findings

### B1. Authentic artifact collection cannot map

`LiveArtifact.run_attempt` is explicitly optional because the list-artifacts
API does not expose attempt identity (`github_live_collector.rs:224-235`).
`parse_artifact` always writes `run_attempt: None`
(`github_live_collector.rs:2091-2135`), and the test asserts that behavior at
`github_live_collector.rs:3837`.  The mapper rejects every such artifact at
`g0_live_mapping.rs:939-961` and again at `1076-1095`.

Thus a real `collect_live` result with any artifacts cannot reach a mapped G0
inventory.  The fix must bind each artifact to the exact run/attempt through an
authoritative observation (not copy the latest attempt or self-fill the typed
field), then add multi-attempt/list/archive regression fixtures.

### B2. Emitted dependency graph is not the checker graph contract

`map_dependency_graph` emits workload nodes and dependency nodes whose kind is
the provider dependency kind (`action`, `scanner`, or reusable-workflow), and
edges whose kind is that same dependency kind (`g0_live_mapping.rs:1854-2027`).
The checker permits only typed node kinds `artifact/check/child/package/release/
source/workload/workflow` (`evidence_check.rs:4343-4356`) and requires every
required workload to have `workload-to-check`, plus applicable
`workload-to-child`, `workload-to-release`, and `workload-to-package` edges
(`evidence_check.rs:4481-4554`).

The mapper never derives those check/child/package/release nodes or edges.  A
non-empty workflow dependency graph therefore cannot satisfy the current G0
contract.  Graph completeness must be derived from manifest/workflow/run facts,
not represented as a self-authored dependency-only side graph.

### B3. Source/dependency raw bytes are not bound to their request endpoint

`map_source` matches only object kind, safe digest, and safe bytes
(`g0_live_mapping.rs:1236-1281`).  Its `validate_raw_references` call verifies
existence, completion, and `response_raw_ref` identity, but not
`request.endpoint_or_operation` (`g0_live_mapping.rs:1319-1362`).  A raw body
from a different `/contents` path or revision with identical bytes can thus
satisfy a workflow/dependency source.  `source_api_urls` in the supplement is
only copied, not a gate (`g0_live_mapping.rs:717-889`).

### B4. Artifact raw refs lack list/archive endpoint binding

Artifact refs are accepted with only object kind `workflow_artifacts`
(`g0_live_mapping.rs:939-949`).  The mapper does not require one exact
run-artifact list response and one exact archive response, nor bind either to
the artifact ID/run URL.  The collector does perform archive digest matching
(`github_live_collector.rs:1546-1599`), but the mapper accepts a crafted
`LiveCollection` without that endpoint relation.  The later checker cannot
restore provenance lost at this boundary.

### B5. Reconciliation and workflow-binding raw refs bypass endpoint/duplicate checks

- `map_reconciliation` deliberately ignores `raw_by_id`, and
  `map_revision_state` copies PR/repository refs without request kind/path
  validation (`g0_live_mapping.rs:2030-2095`).
- `map_pull_request` copies workflow-binding refs and tuples without validating
  their raw requests or rejecting duplicate `(workflow_path, revision, event,
  source_sha)`/run-ID bindings (`g0_live_mapping.rs:1576-1605`). The collector
  groups these tuples, but mapper input is not constrained to collector output.
- PR context/App duplicates and check execution/job cardinality are checked
  (`g0_live_mapping.rs:1606-1635,1724-1781`); equivalent mapper-side duplicate
  policy is absent for workflow bindings and main checks.

### B6. Every request's response/raw-body link is not enforced by the mapper

`map_request` requires a non-empty response-ref string but does not look it up
in the merged raw ledger or verify that the referenced raw object has the same
request ID (`g0_live_mapping.rs:539-603`).  `map_g0_inventory_with_supplement`
maps all requests before nested observations but has no all-request response
binding pass (`g0_live_mapping.rs:383-430`).  The checker later performs this
check (`evidence_check.rs:2137-2202`), but a strict mapper must reject malformed
request/raw ledgers before emitting canonical evidence.  This also leaves
unreferenced raw response bodies accepted at the adapter boundary.

### B7. CAS and live integration remain disconnected (scoped integration gap)

The mapper validates digest/length/ref syntax but does not invoke
`RawObjectStore::verify`; only the producer capture path invokes external-store
verification (`github_acquisition.rs:424-432,2243-2296`).  The live CLI writes
`live-collection.json` and `binding-capture.json` but never invokes
`map_g0_inventory` or installs `AuthenticatedClosingCollector`
(`github_live_cli.rs:68-127`; `live_authority.rs:123-161`).  This is fail-closed
today—`--live` cannot authorize—but it is not an integrated live proof path.

## Required next fixes / negative fixtures

1. Make artifact attempt identity authoritative and test two attempts plus
   metadata-list/archive mismatch.
2. Emit/check the canonical typed dependency graph and test each missing edge,
   wrong node kind, dangling edge, and illegal cycle.
3. Bind every source, dependency, artifact, reconciliation, and workflow
   binding raw ref to exact request endpoint, kind, response body, and subject;
   reject wrong-path/same-bytes and duplicate tuple fixtures.
4. Add one mapper-wide request pass requiring every successful HTTP response ref
   to exist, be unique, and point back to that request; test missing,
   cross-request, and unreferenced raw bodies.
5. Wire only through the producer-owned verified CAS/live-authority seam, then
   rerun against a real `LiveCollection`; absence of a captured input is not a
   pass.

No G0/G2 approval.  No release/install/publication claim.
