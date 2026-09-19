# Records contract reconciliation

Observed `2026-09-19T19:39:35Z` UTC. This is an external, read-only audit. It
does not modify the records worktree and does not approve G0.

Compared exact committed records revisions:

- Old: `df9591e6fd7ca5f79ef2f9b493103e905653106a` (`docs: record runtime promotion evidence`).
- Current: `4a9435e7cbeb220f1d2e985ab4d3fac97290dcef` (`docs: enforce canonical checker schema wording`).

The five user findings reconcile as follows.

1. **Stale 31-repository claim — fixed in current wording.** Old
   `SPEC.md:432-435` treated “31 repositories” as the pending live scope.
   Current `SPEC.md:672-680`, `PLAN.md:131-153`, and `STATUS.md:95-99` say the
   scope is 32, preserve 31 unknown access rows, and distinguish timestamped
   historical snapshots from fresh evidence. Residual: the external collector
   and workload/access/graph inputs still require independent binding and
   review; this is not G0 proof.

2. **Manifest spelling aliases — fixed in current docs.** Old
   `PLAN.md:84-89` deliberately retained both spellings. Current
   `PLAN.md:212-216` makes `schema_version`/`manifest_id` canonical, while
   `SPEC.md:164-169` rejects aliases/coercion/fallbacks. Residual: the runtime
   checker/producer must prove rejection; this audit did not run the checker and
   does not certify its dirty worktree.

3. **Graph/access/model binding — contract added, implementation still
   missing.** Current `SPEC.md:127-162` separates the manifest, live snapshot,
   and normalized records artifacts. `SPEC.md:489-539` now requires typed
   workload→child/release/package/required-check edges with provenance and
   status, and explicitly says this is a requirement rather than an
   implementation claim. The committed checker at
   `d9a277938d54d93f06b85ce6e8d406ebb67468c3` has only a
   `dependency_graph_digest`, access scope/gap strings, and model metadata; it
   has no typed graph-edge/node fields and does not bind those digests to the
   required graph/access artifacts. This is a substantive blocker.

4. **Checker completion overclaim — fixed in current wording.** Old
   `SPEC.md:384-392` stated validation behavior as if complete. Current
   `SPEC.md:541-557` labels it a target contract, records the initial semantic
   rejection, and requires independent reviewer attestation before any checker
   result supports a gate. Current `STATUS.md:111-118` likewise marks checker
   work/review incomplete. The d9 implementation has useful exact-scope and
   run checks, but no authoritative fresh-input/independent-pass evidence was
   established here.

5. **Absolute paths — partially fixed only.** Mutable evidence is correctly
   kept outside the source revision and current docs distinguish the external
   evidence root. However, `/Users/...` and `/root/...` paths remain in current
   SPEC/PLAN/STATUS for worktrees, owners, and links. Preserve provenance, but
   add evidence-root-relative references plus an environment-local mapping
   before calling portability complete.

## Exact file hashes

The old/current hashes are in `review.json`. They bind this report to the
committed revisions, not to the records worktree's later uncommitted edits.

## Verdict

`NO_G0_PASS`. Documentation corrected three findings and partially addressed
portability, but typed graph/access artifact binding, authoritative fresh
fleet evidence, strict checker runtime proof, and independent review remain
open.
