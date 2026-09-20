# Immutable workflow-source inventory

Created 2026-09-20T10:21:46Z. Scope: exact accepted source revisions for the canonical 32-repository set. G0 source evidence only; not execution, expected-job projection, check admission, gate input, or workflow migration.

## Body coverage

160 workflow/reusable-action objects indexed. Each row carries repository, accepted source SHA, path, Git blob SHA, raw SHA-256, and immutable body locator. 116 bodies resolve through exact local Git objects; 44 missing-corpus bodies were fetched read-only from GitHub blob endpoints and preserved in per-object api-body-objects files. The newest collector failed closed before all-32 completion and fetched a non-accepted Velnor revision; it was not treated as authoritative.

Historical 155 default workflow seed rows: 141 match accepted repository SHA and Git blob; 14 retained unresolved. These are Velnor seed rows tied to opening source 89f82dd8b287f46a3cf4c0920f341f6ca6c736db rather than accepted d20d4d1d17590cca85b501d982cbaad70d42c641. No alias.

## Recursive references

recursive_workflow_identities follows exact local reusable-workflow references, plus cross-repository references only when the ref equals another accepted source SHA. Branch/tag/external references remain unresolved with reason and ref. Cycles are retained in cycles.ndjson/object rows. Dynamic expressions, missing refs, inaccessible bodies, and out-of-scope repositories remain unresolved.

## Mapper seam

Use workflow-objects.ndjson as source-only joins. Materialize G0WorkflowSource only after preserving exact body bytes and raw-object provenance. Keep workflow source identity distinct from expected workload IDs; this artifact emits no jobs/checks. See owner-handoff.json.

## Integrity

SHA256SUMS covers every artifact file except itself. Verify from this directory with `shasum -a 256 -c SHA256SUMS`.
