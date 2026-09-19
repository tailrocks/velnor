# Exact checker delta review — `ce894a81158706439205b7460811a4f680cbf800`

Review boundary: clean detached worktree
`/private/tmp/velnor-checker-review-ce894`, exact commit
`ce894a81158706439205b7460811a4f680cbf800`, parent
`ded47ef3f35eb690a1d273ec49acc692c376e345`, branch
`codex/github-first-checker`. The later attribution-only tip
`3eafb977eec0cb9e4d2ac4a033cb0f271d1efa63` changes no source files. No
source, owner worktree, remote, or generated project output was changed. This
is an exact bounded source/CLI review, not live collector approval and not a
G0 gate result.

## Verdict

**The CAS reread and fail-closed parser delta is material, but exact approval
remains blocked.** The commit adds an explicit `--evidence-root`, reopens
outer/raw objects, recomputes measured bytes/digests, rejects missing/outside
objects, rejects static job/step conditions and matrices, and propagates
nested child edges. The exact CLI harness verifies those improvements.

Remaining blockers are concrete:

* CAS validation covers only the outer typed snapshot and raw objects. Source
  workflow/action bytes still have only caller-supplied local hashes plus raw
  ID membership; `source-bytes-replay` is accepted. No source Git object/path/
  commit relation is reopened from CAS.
* A leaf symlink inside the root and a hardlink to an outside file containing
  the expected bytes are accepted. `g0_storage_path` canonicalizes a path,
  then later calls `metadata` and `read` on that path, leaving an ancestor
  rename/symlink TOCTOU window. The bounded stress probe found no
  authority-only acceptance during 80 runs/1,470 swaps, but that is not a
  race-proof result; the unsafe sequence remains in source.
* Nested edges are propagated but are not mapped into the reviewed workload:
  the nested fixture now emits two child findings, including an edge for
  workload `inner`, plus a missing dependency-graph obligation. This is a
  false-fail/incomplete graph integration, not complete nested proof.
* An existing foreign graph target with matching declared target SHA/ref is
  still accepted. Wrong source URL path is still accepted.
* `g0-authoritative-proof-missing` remains unconditional. The live collector
  still emits the legacy `SnapshotDocument`, not producer-owned typed G0
  authority.

## Exact verification

Commands run in the detached tree with an external target directory:

```text
CARGO_TARGET_DIR=/private/tmp/velnor-checker-target-ce894 \
  rtk cargo test --locked --all-features --package velnor-tools -- --nocapture
cargo test: 240 passed (1 suite, 4.17s)

CARGO_TARGET_DIR=/private/tmp/velnor-checker-target-ce894 \
  rtk cargo fmt --all -- --check
passed

CARGO_TARGET_DIR=/private/tmp/velnor-checker-target-ce894 \
  rtk cargo clippy --locked --profile test --all-targets --all-features \
    --package velnor-tools -- -D warnings
cargo clippy: No issues found

rtk git diff --check \
  30a85dafcb8cb0830ef128ecf6797b1bf3c60b7f..ce894a81158706439205b7460811a4f680cbf800
passed
```

The exact source worktree was clean. `complete_g0_fixture_round_trips_through_public_check_paths`
passes internally with a temporary CAS, but it calls the Rust helper directly;
it is not an external serialized public CLI artifact.

The existing external `g0-positive-evidence.json` fixture was also tried with
the exact binary and still fails strict parsing before checking:

```text
missing field `evidence_role` at line 73 column 5
```

No current commit artifact supplies a checked-in/generated external 32-row
positive public fixture that reaches a gate pass. The synthetic public fixture
below supplies the current-schema CLI exercise and intentionally remains
blocked by the authority finding.

## Recomputed public CLI harness

Harness and fixtures are outside the source tree:

* harness: [`ce894a81-public-cli-harness/harness.py`](./ce894a81-public-cli-harness/harness.py)
* result: [`ce894a81-public-cli-harness/harness-results.json`](./ce894a81-public-cli-harness/harness-results.json)
* ancestor stress: [`ce894a81-public-cli-harness/toctou_probe.py`](./ce894a81-public-cli-harness/toctou_probe.py)
* stress result: [`ce894a81-public-cli-harness/toctou-results.json`](./ce894a81-public-cli-harness/toctou-results.json)
* serialized cases: [`ce894a81-public-cli-harness/fixtures/`](./ce894a81-public-cli-harness/fixtures/)

The harness recomputes outer/raw digests and populates an explicit
`<case>/store/sha256/<hex>` CAS for ordinary cases before invoking the public
binary with `--evidence-root`. It creates no live facts. Baseline has exactly
`g0-authoritative-proof-missing`.

| Case | Exact result |
| --- | --- |
| baseline with valid CAS root | **pass** structurally; authority blocker only |
| valid 40-hex pin / wrong 40-hex pin | matching ref passes; mismatch emits `g0-workflow-derivation` |
| `@main` action ref | `g0-workflow-derivation` |
| missing next page / wrong next path | `g0-pagination`; wrong path also `g0-request-incomplete` |
| evil API origin | `g0-collector-source` |
| target SHA mismatch | `g0-dependency-edge` |
| foreign graph target with matching target SHA/ref | **false green**; authority blocker only |
| static `if: false` job | `g0-workflow-derivation` (fail closed) |
| static matrix job | `g0-workflow-derivation` (fail closed) |
| nested reusable child | two child findings, but one targets unreviewed `inner`; dependency obligation remains |
| logical `unit` workload / `unit-github` job | `g0-workflow-plan` (current false-fail mapping) |
| missing CAS objects | two `g0-storage-ref` findings (canonicalization cannot resolve; no object is read) |
| tampered captured raw bytes vs old CAS bytes | `g0-raw-object` + `g0-storage-mismatch` |
| wrong source URL path under correct repo prefix | **false green**; authority blocker only |
| source bytes changed with recomputed local hash and raw ID retained | **false green**; authority blocker only |
| in-root symlink to same bytes | **accepted**; authority blocker only |
| leaf symlink outside / ancestor symlink outside | `g0-storage-ref` (outside containment rejected) |
| outside hardlink containing expected bytes | **accepted**; authority blocker only |

The harness exits nonzero only for the two intentional missing semantic guards
(foreign graph target and wrong source URL). Other rows assert the exact
current result, including fail-closed behavior.

## Findings

### F1 — CAS reread does not bind source bytes or every referenced object (blocker)

`check_g0_external_storage` (`evidence_check.rs:1910-2002`) reopens only
`collector_snapshot_storage_ref` and each `G0RawObjectRef.storage_ref`.
`G0WorkflowSource` and `G0ArtifactReference` have no external storage read at
this boundary. Their `bytes_base64`/digest fields are checked internally, and
their `raw_object_refs` are only IDs in the global raw-ID set. The checker does
not compare source bytes with the referenced raw object bytes or require a Git
blob object with exact repository/path/commit identity.

The `source-bytes-replay` case changes workflow source bytes, recomputes the
source hash and full typed snapshot/CAS hash, and retains a valid unrelated
raw ID. It produces only the authority blocker. Thus the new CAS root proves
the serialized snapshot and raw response bytes, but the source identity still
comes from the caller's self-consistent fields.

Required boundary: give every source/artifact object a producer-owned CAS
identity and reopen it through the same safe store helper; bind bytes,
repository, exact path, commit/ref, Git object, and raw request in one chain.

### F2 — CAS path handling accepts in-root symlinks/hardlinks and has TOCTOU

`g0_storage_path` (`evidence_check.rs:2063-2074`) does:

1. `fs::canonicalize(root/sha256/<digest>)`;
2. `canonical.starts_with(root)` containment;
3. later `fs::metadata(&path)` and `fs::read(&path)` (`:2022-2051`).

An outside leaf/ancestor symlink is rejected after canonicalization, as the
harness confirms. But a leaf symlink to an in-root duplicate with the expected
bytes is accepted, and an outside hardlink with those bytes is accepted.
`metadata` follows symlinks, and hardlink provenance/inode ownership is not
checked. More importantly, an attacker can rename the real `sha256` ancestor
after step 1 and replace it with an outside symlink before step 3. The returned
canonical string is then resolved again outside the checked root. The probe
performed 80 CLI runs while swapping the ancestor 1,470 times; it observed no
authority-only result, but timing did not cover every interleaving and cannot
close the source-level race.

Use one descriptor-relative, no-follow, regular-file open and read operation
under a producer-owned root (or an equivalent platform-safe atomic CAS API),
then verify file identity/size/digest from that opened handle. Enforce a
bounded read from the handle, not a pre-check followed by a path reopen. Keep
the agreed layout `<root>/sha256/<lowercase-64-hex>`; do not introduce a
second CAS scheme.

### F3 — Nested edges propagate with the wrong workload identity (high)

The delta adds `plan.child_edges.extend(child_plan.child_edges)` at
`g0_workflow.rs:159`, and the source-only unit test checks that `deep.yml`
appears. In the actual serialized nested case, however, the propagated inner
edge retains the child workflow's workload ID `inner`. The reviewed root
manifest only has workload `scan`, so `check_g0_derived_plan` emits:

* `g0-workflow-child`: source-derived child edge has no reviewed workload
  `inner`;
* `g0-dependency-obligation`: root workload `scan` still lacks its required
  child edge in the independent dependency graph;
* one additional `g0-workflow-child` for the root obligation mismatch.

The parser now preserves a nested edge in memory, but still takes only
`child_plan.jobs.first()` for the reusable target and the checker does not
construct a complete typed root-to-nested relation or reconcile it with the
producer graph. Multiple jobs in a reusable child are therefore not merged
into the parent plan. Map nested edges through the root workload or use typed
qualified workload identities, then require the independent graph to carry
each edge.

### F4 — Foreign graph endpoint remains accepted (high)

The added edge checks (`evidence_check.rs:3950-3984`) only compare declared
edge source/target SHA/ref to the corresponding node fields. A retargeted
required workload-to-check edge pointing at an existing check node in
`tailrocks/velnor-apt`, with target SHA/ref adjusted to that node, passes all
edge checks. The public `graph-wrong-target` case leaves only the authority
finding.

Validate the endpoint relation against typed repository/workload/check policy
identity from the producer-owned graph. Existing node IDs and matching
caller-declared revisions are not enough.

### F5 — Source URL/path and Git object identity remain prefix-only (high)

`source_url_for_repository` (`evidence_check.rs:2584-2586`) still checks only
`starts_with("https://github.com/<repo>/")`. The `source-wrong-path-url` case
points the workflow URL at `other.yml` while keeping the typed path, bytes,
revision, and digest unchanged; it is accepted. The same source/raw-ID replay
gap appears in F1. Parse the URL and compare exact repository/path/commit/raw
endpoint, then verify the Git object bytes from CAS.

### F6 — Fail-closed matrix/condition policy is safe but rejects real modeled
workloads (medium/high)

`g0_workflow.rs:134-145` now rejects any job containing `if` or `strategy.matrix`,
and `action_sources` (`:236-241`) rejects any step containing `if`. The
synthetic static-condition and static-matrix cases correctly emit
`g0-workflow-derivation`; they cannot silently claim a required job exists.

This is not yet positive support for a real fleet whose required jobs use
conditions or matrices. A genuine positive fixture containing such constructs
must either be fully modeled/expanded and reconciled through a typed
workload-to-job map, or remain blocked. The existing external positive fixture
does not reach this code because it is stale under strict schema parsing.

### F7 — Authority gate and live typed producer remain absent (blocker)

`evidence_check.rs:1897-1907` still unconditionally emits
`g0-authoritative-proof-missing`. The live path (`evidence_live.rs:27-61`)
still returns `SnapshotDocument`; it does not produce `G0InventoryEvidence`,
own the CAS objects, capture Git source objects, or independently derive the
full workflow/run graph. Even with a valid reopened CAS, the current CLI
baseline cannot pass G0.

Retain the unconditional blocker until the live producer owns and emits the
typed proof and the checker verifies all required object chains.

### F8 — Missing/large-store and race hardening gaps (medium)

An unresolved path (including a missing object) is reported as
`g0-storage-ref`, because `g0_storage_path` returns `None` before
`check_g0_external_object` can distinguish missing from invalid containment.
That is fail-closed but loses actionable diagnostics. Also, the 128 MiB size
check is performed with `metadata` before `fs::read`; a file can grow after
the check, and there is no total-byte/object-count budget. Use descriptor-bound
reads with per-object and aggregate limits and separate invalid/missing
diagnostics.

## Required implementation boundary before approval

1. Keep the single agreed `<root>/sha256/<hex>` store layout and implement a
   shared safe reader: descriptor-relative/no-follow open, containment,
   regular-file/inode checks, bounded read, digest, and mutation resistance.
2. Reopen every source/artifact/raw/root object from that store and bind exact
   bytes to Git repository/path/commit/blob and request identity.
3. Map nested reusable edges to reviewed root workloads and reconcile all
   nested obligations with the producer-owned graph.
4. Validate graph endpoint relation semantics; reject foreign existing nodes.
5. Add a real serialized 32-row public positive fixture. For required
   conditions/matrices, either model complete expansion and logical mapping or
   fail closed while retaining the authority blocker.
6. Keep `g0-authoritative-proof-missing` until an independent live typed
   collector is wired and proven.

This commit is **not approved** and is not a G0 completion claim.
