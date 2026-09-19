# Final coordinator push-manifest update

Update ID: `20260919T183649Z`  \
Captured: `2026-09-19T18:36:49Z`  \
Parent commit: `d6b54026f74eb301f2c09e62653302d9bc91fa54`  \
Evidence ref: `refs/heads/evidence/github-first-dual-lane-20260919T181508Z`  \
Source base: `abe9ad82a2d4d01b706bbc6122ab6ccb150faad9`

This append-only update preserves the prior report update and captures the
coordinator’s final stable `G0/fleet/pushes.json` snapshot. The source record
reports `captured_at=2026-09-19T18:35:57Z`, SHA-256
`8faaa8af6224e45e4ebf7f908be731680bb37061b31d83b6ce8817ac9aad410f`, and was
unchanged across four reads before copy and during post-copy reconciliation.

The coordinator reported 18/18 task worktrees clean, local/remote equal,
trailers valid, and three historical refs verified. This remains external
evidence only: `attestation=none`, `gate_status=not-evaluated`, and no G0–G7
success is claimed. The manifest records observations, not source approval.

`INVENTORY.tsv` records the three pre-manifest files; `SHA256SUMS` verifies this
update directory except itself. Research/cloned trees, builds/binaries/caches,
raw logs, credentials, mutable session state, and post-capture writes remain
excluded.
