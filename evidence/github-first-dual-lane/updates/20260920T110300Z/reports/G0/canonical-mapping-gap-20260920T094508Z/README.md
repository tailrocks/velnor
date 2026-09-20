# G0 actual-32 mapping gap audit

Created 2026-09-20T09:45:08Z. Read-only external evidence. No source,
workflow, checker, mapper, ref, rule, check, or dispatch mutation.

## Result

The accepted five-partition index proves source-set bookkeeping only:

- 32 unique repositories; five disjoint partitions: 8 + 6 + 6 + 7 + 5.
- 194 expected workload IDs.
- actual_jobs is null for all 32. The four Skills categories are logical
  source obligations, not observed jobs.
- 31/32 accepted default SHAs join the frozen opening identity. Velnor is the
  exception: index d20d4d1d17590cca85b501d982cbaad70d42c641; raw opening
  89f82dd8b287f46a3cf4c0920f341f6ca6c736db; raw closing
  58b5c8122bb7aa2c0431dfea64bd7daf50c55624.
- 31/32 accepted SHAs have a frozen default check target/body. Velnor has only
  the raw-before target. The raw page is an observation, not a typed check
  producer.
- 21 repositories have 155 accepted-revision workflow seed rows. The reviewed
  frozen capture retains zero verified immutable default workflow source bytes.
  Six older sample source body files are directory-listing JSON with the same
  bytes despite different listed blob SHAs; they are rejected as source YAML.
  No workflow body is invented.
  The older sample manifest is
  G0/fleet/g0-current-workflow-ruleset-raw-capture-20260920T051900Z/workflow-sources.ndjson
  (baedd793d9225814d6d1f914aa0ad0058ac62d30c892a350734689531db46f01).
- 185 historical run observations at accepted head exist across 20 repositories.
  They are not current-main execution proof: checkout attestation, role/event
  closure, and all-32 coverage are absent.
- Opening/closing identity is not stable. same_identity=false; default heads
  changed for jackin-project/homebrew-tap and tailrocks/velnor, with PR churn
  also recorded.

Therefore no ManifestDocument, SnapshotDocument, or G0CollectorSnapshot
candidate is schema-valid. candidate.json intentionally contains
candidate_for_checker: null. ready-rows.ndjson is a separate
source/raw-observation row schema.

## Artifacts

Machine records:

- ready-rows.ndjson: 32 independently derived rows. Identity, partition,
  source workload IDs, optional source-tree digests, raw check target refs, and
  raw historical counts are explicit. actual_jobs remains null.
- field-join-ledger.json: 39 checker/mapper fields, 10 join assertions, 14
  concrete missing producers. Statuses distinguish ready source facts, raw-only
  observations, partial partition shape, blocked joins, and missing producers.
- candidate.json: explicit schema mismatch and non-claim boundary.
- owner-handoff.json: checker/mapper canonical input seam and join keys.

The accepted source index is
G0/partition-index-20260920T073424Z/index.json
(43cb2affeca84e59379844bcef1d53d7ead2e1b9596c36bb3554ec012d7c72ca).
Its independent source-only review is
G1/reviews/g0-partition-index-43cb-independent.md
(6e5e95e01ceae0cc903575f9136e86c83249847ba412f4945210f000ed720b92).

The raw capture is
G0/g0-run-metadata-capture-20260920T065449Z/raw-manifest.json
(6cc98c2886ad23a72bd17cf72f0fb5cea9038da8f01090579360affe62d53637), with
independent bounded-observation review
G1/reviews/g0-run-metadata-capture-6cc98-independent.md
(5b5545de1b398e3f263da87937ccbd823b8fb28b1ccff026aeb439dd82f43609).

The schema seams audited:

- checker interface commit 101aa26b849d507d6bdc702d55619bb830c65706,
  current worktree head observed 2b042d26365ac8befb269fe264a5c1143baf66e0;
- mapper interface commit 5c2877dd80c825a8cfa9438a004f0f5e141537c5,
  current worktree head observed 6dd5cd28ddee3d8d3ecc87678eb421eb9ab2f9f0.

## Missing producers

The field ledger binds each task to the exact artifact and join key. The
short list:

1. Fresh stable 32-repository opening/closing capture at accepted revisions.
2. Exact default workflow bytes and recursive reusable/action/scanner closure.
3. All-32 ruleset/detail and required context/App-ID capture.
4. Canonical typed 32-row manifest projection, preserving unknown states.
5. Current-main run role, attempt, runner, and actual-checkout proof.
6. Typed check producer App/suite/run/job/source/checkout joins.
7. Attempt-bound artifact member/archive bindings.
8. All-32 dependency graph nodes/edges with stable IDs and source refs.
9. Bound model.session, workload.artifact, access/rate-limit raw objects.
10. Typed request/raw-object CAS ledger and canonical snapshot bytes/storage ref.

No execution gate, hosted result, release result, or migration claim follows
from this artifact. This is integration planning evidence only.
