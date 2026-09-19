# External evidence update

Update ID: `20260919T182822Z`  \
Captured: `2026-09-19T18:28:22Z`  \
Parent checkpoint commit: `cc683fc934d80bf18e7f87bc585950edb0540508`  \
Evidence ref: `refs/heads/evidence/github-first-dual-lane-20260919T181508Z`  \
Source base: `abe9ad82a2d4d01b706bbc6122ab6ccb150faad9`

## Boundary

This directory appends a new external evidence snapshot to the prior
checkpoint. The prior files and manifests remain unchanged. This update is not
a source-attested revision and does **not** claim success for G0, G1, G2, G3,
G4, G5, G6, or G7. A clean commit, hash manifest, review verdict, or published
evidence ref is not a gate result.

## Included

- New checker-v2 handoff, Skills remediation/baseline/final records, bootstrap
  isolation/legacy records, Homebrew reviews, native/Rust reviews, and preview
  publication result.
- APT discovery implementation/reviews, the exact 8c19fab synthetic review
  inputs, the compact adversarial-v2 top-level result files, and the harness.
- The fleet push snapshot was observed moving during this capture and is
  intentionally excluded; it is recorded as a live-write exclusion below.

The reports retain their source-relative paths under `reports/`. `INVENTORY.tsv`
records destination, source-relative path, size, and SHA-256. `SHA256SUMS`
authenticates this update directory except itself.

## Excluded and timing

The mutable session ledger and live operational state remain excluded. Research
trees/clones, build/debug/target outputs, binaries, caches, raw logs, full
synthetic asset trees, and transient output remain excluded. Any source file
written after `2026-09-19T18:28:22Z` or changed during reconciliation is outside
this update. Specifically, `G0/fleet/pushes.json` changed between copy and
reconciliation and is excluded. Source originals were read only and were not
edited. The update is an append-only evidence record; it must not be used as a
source verification loop.
