# Exact typed G0 contract review — `a613e04108d088e6d5159aa3f3a593d7ab03e777`

Review boundary: clean detached worktree `/private/tmp/g1-g0-contract-a613`,
exact commit `a613e04108d088e6d5159aa3f3a593d7ab03e777` (`feat(checker): add
typed G0 provenance contract`). No source, owner worktree, remote, or generated
output was changed. This is a source/schema review only. It is not live G0
collector approval and not a G0–G7 gate result.

## Verdict

**Do not approve the typed G0 contract for authoritative use yet.** The change
does add strict collector-owned types, fixed-scope checks, digest syntax checks,
raw-reference existence checks, pagination continuity, PR/check/workflow shape
checks, graph cycle/dangling checks, model settings, access census, and the
intentional `g0-authoritative-proof-missing` blocker. The exact test/clippy/fmt
surface is green.

The contract still validates mostly self-attested shapes. It does not bind the
raw bytes, query semantics, API scopes, collector rows, PR before/after state,
ruleset/check producer identities, graph source/relation obligations, or
artifact bytes to independent public facts. No complete typed `collector_snapshot`
fixture exercises the public `evidence-check` path. These are blockers for a
future collector integration even though the current unconditional G0 blocker
correctly prevents a false gate pass.

## Inputs and exact references

- Goal: `velnor-github-first-dual-lane-goal.md`, SHA-256
  `c9bdb89b302b3fc1ebfb8ff43123590f425f03dbd8855f62bf3a2b42b648b647`.
- Provenance handoff: `G0/checker-v2-handoff.md`, SHA-256
  `65b50a6a7344d9f0ee7520e6586e339279dde39d8ed49eed680182ac836e140a`.
- Prior immutable checker review: `G0/checker-review/report.md`, SHA-256
  `0df74945c639900f752bd9d7dafa88cac368fdaca53b86ddc2c9ae4b6477ad04`.
- Prior acceptance matrix: `G0/checker-v2-review/acceptance-matrix-audit.md`,
  SHA-256 `17ae30b6848fe663132d2a237972ef88c3777964e2e285eb94ca3184fcfa4f36`.
- Source commit: [`a613e041`](https://github.com/tailrocks/velnor/blob/a613e04108d088e6d5159aa3f3a593d7ab03e777/crates/velnor-tools/src/evidence_check.rs).
- Typed model: [`g0_contract.rs`](https://github.com/tailrocks/velnor/blob/a613e04108d088e6d5159aa3f3a593d7ab03e777/crates/velnor-tools/src/g0_contract.rs).
- Contract docs: [`evidence-schema.md`](https://github.com/tailrocks/velnor/blob/a613e04108d088e6d5159aa3f3a593d7ab03e777/docs/ci/github-first-dual-lane/evidence-schema.md).

## Observed correct boundaries

1. `check_documents` invokes `check_g0_inventory` only for stage G0
   (`evidence_check.rs:953-972`). A missing typed object produces
   `missing-g0-inventory` (`:1651-1665`).
2. A structurally valid typed object still receives unconditional
   `g0-authoritative-proof-missing` (`:1714-1724`). This is correct while the
   live collector is not wired to this contract.
3. `evidence_live::collect_live_snapshot` still returns the old
   `SnapshotDocument` (`evidence_live.rs:23-63`); it does not emit requests,
   raw objects, typed rulesets/PR producers, dependency graph, model session,
   access rows, or the workload artifact. The docs explicitly retain this
   collector gap (`evidence-schema.md:182-220`). Therefore this review does not
   treat offline typed data as live proof.
4. All typed objects use `serde(deny_unknown_fields)` (`g0_contract.rs:7-355`).
   Legacy scalar G0 fields are not accepted by the new type.

## Exact verification

From the clean detached tree, using an external target directory:

```text
CARGO_TARGET_DIR=/private/tmp/g1-g0-contract-target rtk cargo test -p velnor-tools --no-fail-fast
cargo test: 232 passed (1 suite, 5.16s)

rtk cargo fmt --all -- --check
passed

CARGO_TARGET_DIR=/private/tmp/g1-g0-contract-target rtk cargo clippy -p velnor-tools --all-targets --all-features -- -D warnings
cargo clippy: No issues found
```

Focused direct-helper tests also passed (each 1 passed, 231 filtered):

- `fixed_scope_positive_control_has_exact_membership`
- `legacy_scalar_inventory_shape_is_rejected`
- `g0_request_contract_rejects_unknown_provenance_field`
- `g0_pagination_missing_next_page_fails_closed`
- `g0_graph_dangling_edge_and_wrong_model_fail_closed`
- `g0_completion_rejects_blocker_before_inventory_return`
- `strict_digest_and_target_parsing_rejects_coercion`

The old adversarial `g0-positive-evidence.json` contains the former flat
`g0_inventory` object, not `collector_snapshot`. Running it through the exact
new binary fails strict parsing before validation:

```text
parse strict evidence JSON .../g0-positive-evidence.json
missing field `evidence_role`
```

The fixture tree contains no non-null typed `g0_inventory.collector_snapshot`
positive/negative fixture. Thus the 232 tests do not prove a full public
entrypoint positive control followed by one-field mutations of this contract.

## Findings

### F1 — Snapshot and raw-object digests are self-attested, not content-bound (blocker)

`G0InventoryEvidence` carries a digest outside the typed snapshot
(`g0_contract.rs:7-18`). The checker computes the expected digest from the same
deserialized caller object and compares it (`evidence_check.rs:1684-1695`). A
producer can mutate any field and recompute the digest; there is no external
producer artifact, source revision, immutable object bytes, or reviewer-bound
digest that would detect that rewrite.

`G0RawObjectRef` records only `sha256`, `byte_length`, `media_type`, and an
arbitrary `storage_ref` (`g0_contract.rs:135-147`). The validator checks digest
syntax/nonzero length and request-ID membership (`evidence_check.rs:1885-1903`),
then checks that nested refs name an existing raw ID (`:1950-1985`). It never
reads raw bytes, recomputes the raw hash, verifies that a response ref belongs
to the same request, or binds an artifact digest to its raw object bytes
(`:1904-1947`, `:1987-2013`). `query_sha256`, `variables_sha256`, artifact
SHA-256, and generated-state SHA-256 are likewise only syntactic digests.

Adversarial mutation that remains structurally acceptable: replace a raw
object's content in its external store, or replace its digest/storage ref and
recompute the outer snapshot digest. The current checker cannot distinguish it.
Required fix: producer-owned immutable raw-object bytes plus an externally bound
snapshot/manifest digest; verify every ref's request owner and recomputed bytes.

### F2 — Request method, endpoint, pagination, and safe scopes are not validated (blocker)

Request fields are present (`g0_contract.rs:79-101`), but
`check_g0_request_provenance` accepts any non-empty `method` and
`endpoint_or_operation`, any 2xx status, and any valid-looking query/variable
digest (`evidence_check.rs:1802-1838`). It does not require REST `GET`, a
read-only GraphQL operation, the GitHub API base, an allowed endpoint/query, or
an endpoint/repository binding. A malicious `DELETE` plus
`https://evil.example/query` can therefore pass this validator if the other
shape fields are populated.

Pagination detects repeated page numbers and a missing immediately following
page (`:1840-1883`), but does not require a stream to start at page 1, validate
`link_next`/cursor targets, or bind the next request to the prior link/query.

Auth validation requires provider `github`, nonempty viewer/scope strings, and
a self-declared `secret_excluded` boolean (`:1727-1758`). Per-repository access
only requires `state == complete`, nonempty arbitrary scopes, and empty gaps
(`:3004-3051`). No allowed-scope vocabulary, auth-to-request binding, or
repository/API permission proof exists. Required fix: typed read-only request
semantics, endpoint/query allowlist, complete page-chain identity, and
whitelisted non-secret scopes bound to the collector viewer.

### F3 — Collector rows are not reconciled to the complete independent snapshot (blocker)

Repository validation compares only `default_branch` and
`default_branch_sha` to the manifest/snapshot (`evidence_check.rs:2111-2149`).
The collector's `repository_id`, rulesets/apps, workflow inventory, main-check
producer IDs, and workflow dependency rows are not compared to the independent
snapshot's corresponding facts. A unique invented repository ID or a different
ruleset/workflow graph can pass.

PR comparison is limited to head/base/tested-merge/merge-group SHA
(`:2634-2655`). Collector `state`, draft flag, author/bot identity,
head-repository/fork, trust, applicability, workflow path/revision, and producer
facts are not cross-bound to the snapshot PR row. `check_g0_pull_request`
explicitly permits producer/binding source SHA to be any of head, base, tested
merge, or merge-group and accepts any allowed event (`:2511-2600`); a base push
or diagnostic/manual event can be represented as PR proof.

Reconciliation only checks that its own pre/post maps are equal and cover 32
names (`:2658-2708`). It does not compare pre/post branch or PR tuples to
`collector.repositories`, the reviewed manifest, or the independent snapshot.
An old self-matching before/after capture therefore remains acceptable.

Required fix: compare every typed collector identity to independent snapshot
facts; bind PR head repository/trust and exact required event/candidate; bind
reconciliation rows to the current repository/PR maps; reject stale self-match.

### F4 — Ruleset Apps and workflow/check producers are shape-only (blocker)

Ruleset context/app pairs are compared as sets with the manifest
(`evidence_check.rs:2310-2324`), and each required row checks nonempty context,
nonempty app ID, owning ruleset ID, and raw-ref presence (`:2166-2205`). This
does not validate numeric/known public App identity, uniqueness of ruleset IDs
or required checks, or equality with independently collected ruleset objects.

Main and PR check producers require positive suite/run/job IDs, source SHA,
event, completed/success status, and a repository-prefixed HTTPS URL
(`:2375-2451`, `:2572-2632`). IDs are not cross-related (check run ↔ suite ↔
workflow run ↔ job), URL paths are not checked as API/check URLs, and PR
producer source/event is not constrained to the tested merge candidate and
`pull_request` association. A set of invented positive IDs and a same-repo
`https://github.com/repo/anything` URL can satisfy the shape.

Workflow rows require path/revision/source SHA/events and generated-state shape;
dependencies require nonempty kind/path/revision (`:2216-2281`). There is no
duplicate workflow identity check, dependency-kind allowlist, immutable source
URL for dependencies, or required producer/job graph binding. `workflow.events`
accepts `workflow_dispatch` and `workflow_run` without proving the event is the
required producer event for the reviewed workflow.

### F5 — Dependency graph lacks source/relation obligations (blocker)

`G0GraphNode` has no source revision field (`g0_contract.rs:277-302`). The
validator catches duplicate node IDs, dangling/self edges, and cycles, and
requires each reviewed workload to have one matching repository/workload node
(`evidence_check.rs:2802-2920`). It does not constrain node `kind` to a known
set, compare node applicability to the manifest, require workload nodes to be
typed as `kind == workload`, require edge kinds/required flags, or enforce
workload→child/release/package/required-check obligations. Duplicate edges are
accepted. A node with a matching workload ID but an arbitrary kind and a
separate harmless edge can evade workload-edge enforcement.

Required fix: add source revision/observation/evidence identity to nodes/edges,
define allowed node/edge kinds, compare graph against the reviewed plan, reject
duplicate edge identities, and require each authoritative obligation.

### F6 — Artifact and model/access references have no independent public binding (high)

Artifact validation checks only nonempty name/schema, any HTTPS URL, a
`sha256:<64 lowercase hex>` string, timestamp, and existing raw refs
(`evidence_check.rs:1987-2013`). It does not recompute or compare the workload
artifact to the manifest's expected workload IDs/generated plan digest or raw
artifact bytes. Model validation checks hard-coded Astra/low and Luna/max
strings plus nonempty agent IDs/unique IDs (`:2953-3001`), but has no session
config/raw source binding. Access checks cover 32 names and reject gaps, but
scope strings remain caller-controlled (`:3004-3051`).

## Adversarial matrix

| Mutation | Current exact behavior | Status |
| --- | --- | --- |
| Replace one canonical repository / duplicate repository row | `g0-scope` from exact 32 set/length; duplicate IDs also checked | Covered |
| Duplicate raw ID / request ID / graph node ID / agent ID | Rejected by existing set checks | Covered |
| Duplicate ruleset ID, workflow identity, graph edge, or check API ID | No corresponding uniqueness check | Gap |
| Omit required raw ref / point ref at unknown ID | `g0-raw-reference` | Covered shape only |
| Change raw bytes while preserving/recomputing claimed SHA | No bytes read; can pass shape/digest | False-green risk |
| `method=DELETE`, evil endpoint, arbitrary query digest | Nonempty fields pass | False-green risk |
| Missing page 2 after `has_next_page=true` | `g0-pagination` | Covered |
| Start at page 2 / malicious next link / stream rebinding | No check | Gap |
| Empty/unknown access scope, auth mismatch | Only empty list/gaps rejected | False-green risk |
| Stale self-matching pre/post branch+PR rows | Equality passes; no current cross-bind | False-green risk |
| Wrong PR fork/draft/author/trust/applicability | Not cross-bound to snapshot | False-green risk |
| Ruleset App/check IDs invented but context/app set matches | Shape passes | False-green risk |
| Wrong producer event/source among allowed PR SHAs | Allowed by `:2511-2600` | False-green risk |
| Graph node source revision/kind/required relation mismatch | Source/relation fields absent or unconstrained | False-green risk |
| Artifact digest changed with outer snapshot digest | Format passes; no bytes/manifest binding | False-green risk |
| Flat legacy G0 fixture | Strict parse fails (`missing field evidence_role` before old flat object is usable) | Expected migration rejection |

## Required next slice

1. Keep `g0-authoritative-proof-missing` until an actual read-only collector
   emits this typed contract. Do not convert offline typed fixture success into
   G0 approval.
2. Add a producer-owned, full 32-row typed fixture with one valid positive
   entrypoint run and one-field mutation cases for every row above. Include raw
   object bytes/fixture digests, canonical query/page records, PR before/after,
   ruleset Apps, required producers, graph source/relation edges, model/access,
   and workload artifact references. Assert all mutations fail for their own
   reason, not merely because of the unconditional blocker.
3. Strengthen the validator at the boundary: strict duplicate-key parsing;
   externally bound canonical snapshot/raw bytes; request method/endpoint/query
   semantics and safe scope allowlists; complete collector↔snapshot PR/ruleset/
   workflow/repository reconciliation; exact PR event/candidate checks;
   ruleset/check producer ID relationships; graph source/relation obligations;
   artifact/raw-byte digest equality.
4. Wire `evidence_live` to emit the contract only after it captures complete
   endpoint pages and raw provenance. The current live collector's workflow
   event/source and checkout fields are not sufficient independently: it emits
   workflow event `unknown` (`evidence_live.rs:153-201`) and equates
   `actual_checkout_sha` with run `head_sha` (`:343-352`).

**Disposition:** exact source hygiene passes. Typed G0 semantic review rejects
approval; no live collector proof and no gate pass.
