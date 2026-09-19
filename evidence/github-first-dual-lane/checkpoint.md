# External evidence checkpoint

Checkpoint ID: `github-first-dual-lane-20260919T181508Z`  \
Captured: `2026-09-19T18:17:51Z`  \
Ref: `refs/heads/evidence/github-first-dual-lane-20260919T181508Z`  \
Source base: `abe9ad82a2d4d01b706bbc6122ab6ccb150faad9`

## Boundary

This is a frozen external evidence package. It is **not** a source-attested revision and does **not** claim success for G0, G1, G2, G3, G4, G5, G6, or G7. A clean commit, hash manifest, or published ref is not a gate result.

The package preserves selected stable reports, JSON/TSV snapshots, and synthetic hostile G2 fixtures from the live evidence root. Source originals were read only and were not edited. Evidence written after the capture time, mutable session state, and transient outputs are outside this checkpoint.

## Included

- G0 checker review, checker-job findings, check-contract records, and the hostile G2 fixture harness with manifest, snapshot, evidence inputs, result reports, and TSV results.
- G0 fleet requirements and verification snapshots, context/handoff records, revisions, PR checks, inventory/configuration/open-PR TSVs, workload/stale-run records, and focused bootstrap/cache/hosted/distribution/native/runtime/consumer/skills/action reports.
- G1 integration, scan-integrity, bootstrap, seed-pin, hosted-provider, and runtime-product-audit records.
- G2 preview-publication design.

`INVENTORY.tsv` lists each included file, source-relative path, size, and SHA-256. `SHA256SUMS` authenticates the checkpoint contents except itself.

## Excluded

Research trees and clones; build targets and binaries; debug outputs; dependency caches and node_modules; raw/huge logs; credentials and private material; mutable `G0/fleet/session.json`; and files still being written after capture. These exclusions prevent a moving live workspace from being represented as a stable attestation. The originals remain in the external evidence workspace.

Reviewed references recorded for context: checker `b3b6b2ef5239ff3354f504b8aeb638129fd0504b`; records docs `651c69ccc7d5d6aab3e7d63ddd8c936b491bfdb5`; prior records docs `df9591e6fd7ca5f79ef2f9b493103e905653106a`. The independent checker review is `G0/checker-review/report.md`.
